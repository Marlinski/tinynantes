//! Tiny Nantes server.
//!
//! - serves the viewer and the tile pyramids (web/)
//! - keeps the city's live feeds fresh on its own timers and serves cached copies, so visitors never hit the
//!   upstream APIs: Naolib timetable (GTFS, daily) + delays (GTFS-RT, every 30 s), road traffic (every 3 min),
//!   events (hourly). When an upstream fails, the last good copy keeps being served, with its age.
//!
//! - planes (ADS-B), SNCF trains and the station board, players (sign-in, WebSocket positions, logged chat),
//!   places (crawled menus) in SQLite
//!
//! Env: TN_WEB (default ../web), TN_DATA (cache + SQLite, default ./data), TN_PLACES (crawled places, default
//! TN_DATA/places), TN_PORT (default 8080), RUST_LOG.

mod bikes;
mod feeds;
mod gtfs;
mod l93;
mod menus;
mod news;
mod people;
mod planes;
mod players;
mod rt;
mod sim;
mod sncf;
mod store;
mod talk;

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, Query, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures_util::StreamExt;
use prost::Message;
use serde::Serialize;
use tokio::sync::RwLock;
use tower_http::compression::CompressionLayer;
use tower_http::services::ServeDir;

const WEATHER_URL: &str = "https://api.open-meteo.com/v1/forecast?latitude=47.218&longitude=-1.553&current=temperature_2m,precipitation,rain,snowfall,weather_code,cloud_cover,wind_speed_10m,wind_direction_10m,is_day,visibility&daily=sunrise,sunset&timezone=Europe%2FParis&forecast_days=1";
const RT_URL: &str = "https://api.okina.fr/gateway/semgtfsrt/realtime/trip-updates/NAOLIBORG";
const USER_AGENT: &str = concat!("tinynantes-server/", env!("CARGO_PKG_VERSION"));

/// A value fetched from upstream, with when it was last refreshed and the last error if any.
struct Cached<T> {
    value: Option<T>,
    ok_at: Option<i64>,
    error: Option<String>,
}
impl<T> Default for Cached<T> {
    fn default() -> Self {
        Self { value: None, ok_at: None, error: None }
    }
}
impl<T> Cached<T> {
    fn set(&mut self, r: anyhow::Result<T>) {
        match r {
            Ok(v) => {
                self.value = Some(v);
                self.ok_at = Some(now());
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }
    fn age(&self) -> Option<i64> {
        self.ok_at.map(|t| now() - t)
    }
}

#[derive(Default)]
struct Realtime {
    delays: HashMap<usize, Vec<i32>>,
    canceled: HashSet<usize>,
    matched: usize,
}

struct App {
    http: reqwest::Client,
    origin: feeds::Origin,
    web: PathBuf,
    data: PathBuf,
    timetable: RwLock<Cached<Arc<gtfs::Timetable>>>,
    rt: RwLock<Cached<Realtime>>,
    traffic: RwLock<Cached<Bytes>>,
    events: RwLock<Cached<Bytes>>,
    weather: RwLock<Cached<Bytes>>,
    vehicles: RwLock<(i64, Bytes)>,
    sky: Arc<RwLock<planes::Sky>>,
    sncf: RwLock<Cached<Arc<sncf::Day>>>,
    sncf_delays: RwLock<Cached<HashMap<usize, Vec<i32>>>>,
    store: Arc<store::Store>,
    menus: Arc<menus::Fetcher>,
    /// places of the map (pois.json): id -> (name, kind, website)
    pois: Arc<HashMap<String, (String, String, Option<String>)>>,
    news: Arc<RwLock<news::News>>,
    bikes: Arc<RwLock<bikes::Bikes>>,
    talker: Arc<talk::Talker>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    // `tinynantes-server menu <url>`: read one website the way the server does and print the result
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 3 && args[1] == "menu-lines" {
        for (l, h) in menus::debug_lines(&menus::Fetcher::new()?, &args[2]).await {
            println!("{}{l}", if h { "# " } else { "" });
        }
        return Ok(());
    }
    if args.len() == 3 && args[1] == "menu" {
        let f = menus::Fetcher::new()?;
        let v = menus::fetch_place(&f, "test", "", "", &args[2]).await;
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let web = PathBuf::from(std::env::var("TN_WEB").unwrap_or_else(|_| "../web".into()));
    if args.len() >= 2 && args[1] == "menus-crawl" {
        // read every place with a website now (stats at the end); later reads stay lazy/nightly
        let data = PathBuf::from(std::env::var("TN_DATA").unwrap_or_else(|_| "data".into()));
        let places = std::env::var("TN_PLACES").map(PathBuf::from).unwrap_or_else(|_| data.join("places"));
        std::fs::create_dir_all(&places)?;
        let store = Arc::new(store::Store::open(&data.join("tinynantes.db"), places.clone())?);
        let f = Arc::new(menus::Fetcher::new()?);
        let pois = load_pois(&web);
        let todo: Vec<_> = pois.iter().filter(|(_, p)| p.2.is_some()).map(|(id, p)| (id.clone(), p.clone())).collect();
        let n = todo.len();
        let stats = Arc::new(std::sync::Mutex::new(HashMap::<String, usize>::new()));
        let done = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        futures_util::stream::iter(todo).for_each_concurrent(12, |(id, (name, kind, site))| {
            let (f, store, stats, done, places) = (f.clone(), store.clone(), stats.clone(), done.clone(), places.clone());
            async move {
                let mut v = menus::fetch_place(&f, &id, &name, &kind, site.as_deref().unwrap_or("")).await;
                let key = if v.get("error").is_some() { "error".to_string() }
                    else if v["sections"].as_array().is_some_and(|a| !a.is_empty()) { format!("menu:{}", v["source"].as_str().unwrap_or("")) }
                    else if v["images"].as_array().is_some_and(|a| !a.is_empty()) { "images".into() }
                    else if v["pdfs"].as_array().is_some_and(|a| !a.is_empty()) { "pdf links".into() }
                    else if v["links"].as_array().is_some_and(|a| !a.is_empty()) { "platform links".into() }
                    else { "nothing".into() };
                *stats.lock().unwrap().entry(key).or_default() += 1;
                // also as a seed file (shipped in the image: a fresh server has every menu before anyone asks)
                let _ = std::fs::write(places.join(format!("{id}.json")), v.to_string());
                let st = store.clone();
                let _ = tokio::task::spawn_blocking(move || st.save_place(&id, &mut v)).await;
                let k = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if k % 50 == 0 { eprintln!("{k}/{n} {:?}", stats.lock().unwrap()); }
            }
        }).await;
        println!("{n} places: {:?}", stats.lock().unwrap());
        return Ok(());
    }
    let data = PathBuf::from(std::env::var("TN_DATA").unwrap_or_else(|_| "data".into()));
    let port: u16 = std::env::var("TN_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);
    let origin = feeds::origin_from_world(&web.join("data/world/world.json"))?;
    let http = reqwest::Client::builder().user_agent(USER_AGENT).timeout(Duration::from_secs(120)).build()?;
    let places = std::env::var("TN_PLACES").map(PathBuf::from).unwrap_or_else(|_| data.join("places"));
    let store = Arc::new(store::Store::open(&data.join("tinynantes.db"), places)?);
    // last good copies from the previous run: served until the first refresh succeeds
    let restore = |key: &str| {
        let mut c = Cached::<Bytes>::default();
        if let Some((ts, b)) = store.load_feed(key) {
            c.value = Some(Bytes::from(b));
            c.ok_at = Some(ts);
        }
        RwLock::new(c)
    };
    let (traffic0, events0, weather0) = (restore("traffic"), restore("events"), restore("weather"));
    let talker = Arc::new(talk::Talker::new(
        reqwest::Client::builder().user_agent(USER_AGENT).timeout(Duration::from_secs(90)).build()?,
        &data.join("tinynantes.db"))?);
    let app = Arc::new(App {
        store: store.clone(),
        http,
        origin,
        web: web.clone(),
        data,
        timetable: Default::default(),
        rt: Default::default(),
        traffic: traffic0,
        events: events0,
        weather: weather0,
        vehicles: RwLock::new((0, Bytes::new())),
        sky: Default::default(),
        sncf: Default::default(),
        sncf_delays: Default::default(),
        menus: Arc::new(menus::Fetcher::new()?),
        pois: Arc::new(load_pois(&web)),
        news: Default::default(),
        bikes: Default::default(),
        talker,
    });
    let players = players::open(&app.data.join("tinynantes.db"))?;

    tokio::spawn(timetable_loop(app.clone()));
    tokio::spawn(rt_loop(app.clone()));
    tokio::spawn(every(app.clone(), 180, |a| Box::pin(refresh_traffic(a))));
    tokio::spawn(every(app.clone(), 3600, |a| Box::pin(refresh_events(a))));
    tokio::spawn(every(app.clone(), 600, |a| Box::pin(refresh_weather(a))));
    tokio::spawn(planes::run(app.http.clone(), app.origin, app.sky.clone(), store.clone()));
    tokio::spawn(places_loop(store.clone()));
    tokio::spawn(menus_nightly(app.clone()));
    tokio::spawn(sncf_loop(app.clone()));
    tokio::spawn(news::run(app.http.clone(), app.news.clone()));
    tokio::spawn(bikes::run(app.http.clone(), app.origin, app.bikes.clone()));
    let web2 = web.clone();
    let sim = tokio::task::spawn_blocking(move || sim::build(&web2)).await??;
    let world = sim::World::new(sim);
    tokio::spawn(sim::run(world.clone()));
    tokio::spawn(sim_inputs(app.clone(), world.clone()));
    tokio::spawn(bike_departures(app.clone(), world.clone()));

    let api = Router::new()
        .route("/api/transit/vehicles", get(vehicles))
        .route("/api/traffic", get(traffic))
        .route("/api/events", get(events))
        .route("/api/weather", get(weather))
        .route("/api/status", get(status))
        .route("/api/planes", get(planes_h))
        .route("/api/airport", get(airport))
        .route("/api/station", get(station))
        .route("/api/trains", get(trains))
        .route("/api/poi/:id", get(poi))
        .route("/api/places/search", get(places_search))
        .route("/api/places/menus", get(places_menus))
        .route("/healthz", get(|| async { "ok\n" }))
        .with_state(app.clone());
    let social = Router::new()
        .route("/api/auth/login", post(players::login))
        .route("/api/ws", get(players::ws))
        .route("/api/chat/recent", get(players::recent))
        .with_state(players.clone());
    let talking = Router::new()
        .route("/api/talk", post(talk_h))
        .route("/api/talk/end", post(talk_end))
        .route("/api/news", get(news_h))
        .route("/api/bikes", get(bikes_h))
        .with_state(TalkState { app: app.clone(), world: world.clone(), players: players.clone() });
    let living = Router::new()
        .route("/api/world", get(world_ws))
        .route("/api/agent/:id", get(agent))
        .with_state(world);
    let router = api
        .merge(social)
        .merge(living)
        .merge(talking)
        .fallback_service(ServeDir::new(&web).append_index_html_on_directories(true))
        .layer(middleware::from_fn(cache_headers))
        .layer(CompressionLayer::new());
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!("serving {web:?} on :{port}");
    axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

async fn cache_headers(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let mut res = next.run(req).await;
    let v = if path.starts_with("/api/") {
        "no-cache"
    } else if path.ends_with(".json") || path.ends_with(".html") || path == "/" || path.ends_with(".js") || path.ends_with(".css") {
        "no-cache" // revalidate: they change on every publish
    } else if path.starts_with("/data/") {
        "public, max-age=86400"
    } else {
        "public, max-age=3600"
    };
    res.headers_mut().entry(header::CACHE_CONTROL).or_insert(HeaderValue::from_static(v));
    res
}

// ---------------------------------------------------------------- refresh loops

async fn every<F>(app: Arc<App>, secs: u64, f: F)
where
    F: Fn(Arc<App>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
{
    loop {
        f(app.clone()).await;
        tokio::time::sleep(Duration::from_secs(secs)).await;
    }
}

async fn timetable_loop(app: Arc<App>) {
    let mut backoff = 60;
    loop {
        let day = gtfs::today();
        let have = app.timetable.read().await.value.as_ref().map(|t| t.date);
        if have != Some(day) {
            let r = load_timetable(&app, day).await;
            let failed = r.is_err();
            if let Err(e) = &r {
                tracing::warn!("timetable: {e:#}");
            }
            app.timetable.write().await.set(r.map(Arc::new));
            if failed {
                tokio::time::sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(3600);
                continue;
            }
            backoff = 60;
        }
        tokio::time::sleep(Duration::from_secs(300)).await;
    }
}

async fn load_timetable(app: &App, day: chrono::NaiveDate) -> anyhow::Result<gtfs::Timetable> {
    let dir = app.data.join("gtfs");
    gtfs::ensure_gtfs(&app.http, &dir, 24).await?;
    let shapes = gtfs::shapes_from_world(&app.web.join("data/world/transit.json"))?;
    tokio::task::spawn_blocking(move || gtfs::load(&dir, day, &shapes)).await?
}

async fn rt_loop(app: Arc<App>) {
    let mut wait = 30;
    loop {
        let tt = app.timetable.read().await.value.clone();
        if let Some(tt) = tt {
            let r = async {
                let b = app.http.get(RT_URL).send().await?.error_for_status()?.bytes().await?;
                let feed = rt::FeedMessage::decode(b)?;
                let (delays, canceled, matched) = gtfs::apply_rt(&tt, &feed);
                tracing::debug!("rt: {} entities, {matched} matched", feed.entity.len());
                anyhow::Ok(Realtime { delays, canceled, matched })
            }
            .await;
            wait = if r.is_ok() { 30 } else { (wait * 2).min(300) };
            if let Err(e) = &r {
                tracing::warn!("realtime: {e:#}");
            }
            app.rt.write().await.set(r);
        }
        tokio::time::sleep(Duration::from_secs(wait)).await;
    }
}

async fn refresh_traffic(app: Arc<App>) {
    let r = feeds::traffic(&app.http, app.origin).await.and_then(|v| Ok(Bytes::from(serde_json::to_vec(&v)?)));
    if let Err(e) = &r {
        tracing::warn!("traffic: {e:#}");
    }
    if let Ok(b) = &r {
        app.store.save_feed("traffic", now(), b);
    }
    app.traffic.write().await.set(r);
}

async fn refresh_events(app: Arc<App>) {
    let r = feeds::events(&app.http, app.origin).await.and_then(|v| Ok(Bytes::from(serde_json::to_vec(&v)?)));
    if let Err(e) = &r {
        tracing::warn!("events: {e:#}");
    }
    if let Ok(b) = &r {
        app.store.save_feed("events", now(), b);
    }
    app.events.write().await.set(r);
}

/// Current weather in Nantes (Open-Meteo, CC BY 4.0), every 10 min: drives the viewer's rain, fog, clouds...
async fn refresh_weather(app: Arc<App>) {
    let r = async {
        let b = app.http.get(WEATHER_URL).send().await?.error_for_status()?.bytes().await?;
        serde_json::from_slice::<serde_json::Value>(&b)?; // must be JSON
        anyhow::Ok(b)
    }
    .await;
    if let Err(e) = &r {
        tracing::warn!("weather: {e:#}");
    }
    if let Ok(b) = &r {
        app.store.save_feed("weather", now(), b);
    }
    app.weather.write().await.set(r);
}

// ---------------------------------------------------------------- handlers

fn json(b: Bytes) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], b).into_response()
}

/// Wrap a cached JSON array as {"age": s, "items": [...]}, or 503 if never fetched.
async fn wrapped(c: &RwLock<Cached<Bytes>>) -> Response {
    let c = c.read().await;
    match &c.value {
        None => (StatusCode::SERVICE_UNAVAILABLE, "not fetched yet").into_response(),
        Some(b) => {
            let mut out = format!("{{\"age\":{},\"items\":", c.age().unwrap_or(-1)).into_bytes();
            out.extend_from_slice(b);
            out.push(b'}');
            json(out.into())
        }
    }
}

async fn traffic(State(app): State<Arc<App>>) -> Response {
    wrapped(&app.traffic).await
}

async fn events(State(app): State<Arc<App>>) -> Response {
    wrapped(&app.events).await
}

async fn weather(State(app): State<Arc<App>>) -> Response {
    wrapped(&app.weather).await
}

#[derive(Serialize)]
struct VehiclesOut<'a> {
    now: i64,
    /// seconds since the last successful realtime update (None: schedule only)
    rt_age: Option<i64>,
    vehicles: Vec<gtfs::Vehicle<'a>>,
}

/// Running trips with their upcoming stop times; recomputed at most every 10 s and shared by all visitors.
async fn vehicles(State(app): State<Arc<App>>) -> Response {
    let t = now();
    {
        let c = app.vehicles.read().await;
        if t - c.0 < 10 && !c.1.is_empty() {
            return json(c.1.clone());
        }
    }
    let tt = app.timetable.read().await;
    let Some(tt) = tt.value.clone() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "timetable not loaded yet").into_response();
    };
    let rt = app.rt.read().await;
    let empty = Realtime::default();
    let r = rt.value.as_ref().unwrap_or(&empty);
    let out = VehiclesOut { now: t, rt_age: rt.age(), vehicles: gtfs::vehicles(&tt, &r.delays, &r.canceled, t, 60) };
    let b = Bytes::from(serde_json::to_vec(&out).unwrap_or_default());
    *app.vehicles.write().await = (t, b.clone());
    json(b)
}

async fn status(State(app): State<Arc<App>>) -> Response {
    let tt = app.timetable.read().await;
    let rt = app.rt.read().await;
    let tr = app.traffic.read().await;
    let ev = app.events.read().await;
    let v = serde_json::json!({
        "timetable": {"date": tt.value.as_ref().map(|t| t.date.to_string()), "trips": tt.value.as_ref().map(|t| t.trips.len()), "error": tt.error},
        "realtime": {"age": rt.age(), "matched_trips": rt.value.as_ref().map(|r| r.matched), "error": rt.error},
        "traffic": {"age": tr.age(), "error": tr.error},
        "events": {"age": ev.age(), "error": ev.error},
    });
    json(Bytes::from(v.to_string()))
}


// ---------------------------------------------------------------- planes, airport, trains, places

async fn planes_h(State(app): State<Arc<App>>) -> Response {
    let s = app.sky.read().await;
    let v = serde_json::json!({"age": now() - s.at, "items": s.planes, "error": s.error});
    json(Bytes::from(v.to_string()))
}

async fn airport(State(app): State<Arc<App>>) -> Response {
    let s = app.sky.read().await;
    let mut arr = s.arrivals.clone();
    let mut dep = s.departures.clone();
    arr.reverse();
    dep.reverse();
    let (x, y) = l93::project(planes::NTE.0, planes::NTE.1);
    let v = serde_json::json!({"name": "Nantes Atlantique (NTE)", "p": [x - app.origin.0, y - app.origin.1],
        "arrivals": arr, "departures": dep,
        "note": "Mouvements observés en ADS-B (adsb.lol) ; routes : adsbdb.com"});
    json(Bytes::from(v.to_string()))
}

async fn sncf_loop(app: Arc<App>) {
    let mut n = 0u64;
    loop {
        let day = gtfs::today();
        let have = app.sncf.read().await.value.as_ref().map(|d| d.date);
        if have != Some(day) {
            let dir = app.data.join("sncf");
            let o = app.origin;
            let r = async {
                sncf::ensure(&app.http, &dir).await?;
                tokio::task::spawn_blocking(move || sncf::load(&dir, day, o)).await?
            }
            .await;
            if let Err(e) = &r {
                tracing::warn!("sncf: {e:#}");
            }
            app.sncf.write().await.set(r.map(Arc::new));
        }
        if let Some(d) = app.sncf.read().await.value.clone() {
            let r = async {
                let b = app.http.get(sncf::RT_URL).send().await?.error_for_status()?.bytes().await?;
                let feed = rt::FeedMessage::decode(b)?;
                anyhow::Ok(sncf::delays(&d, &feed))
            }
            .await;
            if let Err(e) = &r {
                tracing::warn!("sncf realtime: {e:#}");
            }
            app.sncf_delays.write().await.set(r);
        }
        n += 1;
        tokio::time::sleep(Duration::from_secs(if n < 2 { 5 } else { 60 })).await;
    }
}

async fn station(State(app): State<Arc<App>>) -> Response {
    let Some(d) = app.sncf.read().await.value.clone() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "not loaded yet").into_response();
    };
    let dl = app.sncf_delays.read().await;
    let empty = HashMap::new();
    let b = sncf::board(&d, dl.value.as_ref().unwrap_or(&empty), now());
    json(Bytes::from(serde_json::to_vec(&serde_json::json!({"name": "Nantes", "rt_age": dl.age(), "board": b})).unwrap_or_default()))
}

async fn trains(State(app): State<Arc<App>>) -> Response {
    let Some(d) = app.sncf.read().await.value.clone() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "not loaded yet").into_response();
    };
    let dl = app.sncf_delays.read().await;
    let empty = HashMap::new();
    let t = now();
    let m = sncf::moving(&d, dl.value.as_ref().unwrap_or(&empty), t);
    json(Bytes::from(serde_json::to_vec(&serde_json::json!({"now": t, "items": m})).unwrap_or_default()))
}

/// GET /api/poi/:id -> what the crawler gathered about a place (menu, links), from the SQLite store
async fn poi(State(app): State<Arc<App>>, UrlPath(id): UrlPath<String>) -> Response {
    let ok = id.len() < 24 && id.starts_with(['n', 'w', 'r']) && id[1..].chars().all(|c| c.is_ascii_digit());
    if !ok {
        return (StatusCode::BAD_REQUEST, "bad id").into_response();
    }
    let st = app.store.clone();
    let id2 = id.clone();
    let t = now();
    let got = tokio::task::spawn_blocking(move || st.open_place(&id2, t)).await.ok().flatten();
    let info = app.pois.get(&id).cloned();
    let mut v: serde_json::Value = match (&got, &info) {
        (Some((b, _)), _) => serde_json::from_str(b).unwrap_or_default(),
        (None, Some((name, kind, site))) => serde_json::json!({"id": id, "name": name, "kind": kind, "website": site, "sections": []}),
        (None, None) => return (StatusCode::NOT_FOUND, "{}").into_response(),
    };
    // read the website again when our copy is more than a day old (in the background: answer now)
    let fetched = got.as_ref().and_then(|g| g.1);
    if let Some((name, kind, Some(site))) = info {
        if fetched.map_or(true, |f| t - f > menus::STALE) && app.menus.begin(&id) {
            let app2 = app.clone();
            tokio::spawn(refresh_place(app2, id.clone(), name, kind, site));
        }
    }
    v["refreshing"] = serde_json::json!(app.menus.busy(&id));
    json(Bytes::from(v.to_string()))
}

async fn refresh_place(app: Arc<App>, id: String, name: String, kind: String, site: String) {
    let mut v = menus::fetch_place(&app.menus, &id, &name, &kind, &site).await;
    let st = app.store.clone();
    let id2 = id.clone();
    let _ = tokio::task::spawn_blocking(move || st.save_place(&id2, &mut v)).await;
    app.menus.end(&id);
}

/// Every night at 4:00 (Paris), read again the places people opened in the last 30 days.
async fn menus_nightly(app: Arc<App>) {
    use chrono::TimeZone;
    loop {
        let now_p = chrono::Utc::now().with_timezone(&chrono_tz::Europe::Paris);
        let mut next = chrono_tz::Europe::Paris.from_local_datetime(&now_p.date_naive().and_hms_opt(4, 0, 0).unwrap()).single()
            .unwrap_or(now_p);
        if next <= now_p {
            next = next + chrono::Duration::days(1);
        }
        tokio::time::sleep((next - now_p).to_std().unwrap_or(Duration::from_secs(3600))).await;
        let st = app.store.clone();
        let ids = tokio::task::spawn_blocking(move || st.viewed_since(now() - 30 * 86400)).await.unwrap_or_default();
        tracing::info!("menus: nightly refresh of {} places", ids.len());
        let todo: Vec<_> = ids.into_iter().filter_map(|id| {
            let (name, kind, site) = app.pois.get(&id)?.clone();
            Some((id, name, kind, site?))
        }).collect();
        futures_util::stream::iter(todo).for_each_concurrent(6, |(id, name, kind, site)| {
            let app = app.clone();
            async move {
                if app.menus.begin(&id) {
                    refresh_place(app, id, name, kind, site).await;
                }
            }
        }).await;
    }
}

fn load_pois(web: &std::path::Path) -> HashMap<String, (String, String, Option<String>)> {
    let Ok(b) = std::fs::read(web.join("data/world/pois.json")) else { return HashMap::new() };
    let v: Vec<serde_json::Value> = serde_json::from_slice(&b).unwrap_or_default();
    v.into_iter().filter_map(|p| {
        let t = &p["t"];
        let site = t["website"].as_str().or(t["contact:website"].as_str()).map(str::to_string);
        Some((p["id"].as_str()?.to_string(), (t["name"].as_str().unwrap_or("").to_string(), p["k"].as_str().unwrap_or("").to_string(), site)))
    }).collect()
}

#[derive(serde::Deserialize)]
struct SearchQ {
    q: String,
}

/// GET /api/places/search?q=galette -> places whose name or menu matches
async fn places_search(State(app): State<Arc<App>>, Query(q): Query<SearchQ>) -> Response {
    if q.q.len() > 100 {
        return (StatusCode::BAD_REQUEST, "query too long").into_response();
    }
    let st = app.store.clone();
    let v = tokio::task::spawn_blocking(move || st.search(&q.q, 30)).await.unwrap_or_default();
    json(Bytes::from(serde_json::to_vec(&v).unwrap_or_default()))
}

/// GET /api/places/menus -> ids of places with a menu (the viewer marks them)
async fn places_menus(State(app): State<Arc<App>>) -> Response {
    let st = app.store.clone();
    let v = tokio::task::spawn_blocking(move || st.place_ids_with_menu()).await.unwrap_or_default();
    json(Bytes::from(serde_json::to_vec(&v).unwrap_or_default()))
}

async fn places_loop(st: Arc<store::Store>) {
    loop {
        let s = st.clone();
        match tokio::task::spawn_blocking(move || s.import_places()).await {
            Ok(Ok(n)) if n > 0 => tracing::info!("places: imported {n}"),
            Ok(Err(e)) => tracing::warn!("places: {e:#}"),
            _ => {}
        }
        tokio::time::sleep(Duration::from_secs(300)).await;
    }
}


// ---------------------------------------------------------------- the simulated inhabitants

/// Feed the simulation with the measured traffic and the weather, every minute.
async fn sim_inputs(app: Arc<App>, w: Arc<sim::World>) {
    #[derive(serde::Deserialize)]
    struct TransitFile {
        routes: Vec<people::RouteIn>,
        shapes: HashMap<String, people::ShapeIn>,
    }
    let tf: Option<Arc<TransitFile>> = std::fs::read(app.web.join("data/world/transit.json")).ok()
        .and_then(|b| serde_json::from_slice(&b).ok()).map(Arc::new);
    let stations: Vec<(String, [f32; 2])> = std::fs::read(app.web.join("data/world/transit.json")).ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .map(|v| v["stations"].as_array().cloned().unwrap_or_default().iter().filter_map(|s| {
            Some((s["name"].as_str()?.to_string(), [s["p"][0].as_f64()? as f32, s["p"][1].as_f64()? as f32]))
        }).collect()).unwrap_or_default();
    // a demonstration's usual route: Commerce, cours des 50-Otages, rue de Strasbourg, the château, Bouffay
    let route: Vec<[f32; 2]> = ["Commerce", "50 Otages", "Cirque - Marais", "St-Pierre", "Duchesse Anne - Château", "Bouffay", "Commerce"]
        .iter().filter_map(|n| stations.iter().find(|s| s.0 == *n).map(|s| s.1)).collect();
    let mut built: Option<chrono::NaiveDate> = None;
    loop {
        // today's timetable, indexed for the inhabitants' trips
        let tt = app.timetable.read().await.value.clone();
        if let (Some(tt), Some(tf)) = (tt, tf.clone()) {
            if built != Some(tt.date) {
                built = Some(tt.date);
                if let Ok(ix) = tokio::task::spawn_blocking(move || people::TransitIndex::build(tt, &tf.shapes, &tf.routes)).await {
                    w.sim.write().await.set_transit(Arc::new(ix));
                    tracing::info!("sim: transit index ready");
                }
            }
        }
        // a demonstration: announced for today in the local news (weekend afternoons), or forced for testing
        let paris = chrono::Utc::now().with_timezone(&chrono_tz::Europe::Paris);
        use chrono::{Datelike, Timelike};
        let weekend = paris.weekday().num_days_from_monday() >= 5;
        let forced = std::env::var("TN_FORCE_MARCH").ok().filter(|v| !v.is_empty());
        let announced = app.news.read().await.march.clone();
        let march = match (forced, announced) {
            (Some(m), _) => Some((m, now() as f64 + 3600.0)),
            (None, Some(m)) if weekend && (14..18).contains(&paris.hour()) => {
                let end = paris.date_naive().and_hms_opt(17, 45, 0).and_then(|d| d.and_local_timezone(chrono_tz::Europe::Paris).single());
                Some((m, end.map(|e| e.timestamp() as f64).unwrap_or(now() as f64 + 3600.0)))
            }
            _ => None,
        };
        if route.len() >= 3 {
            w.sim.write().await.set_march(march.map(|(m, until)| (m, route.clone(), until)));
        }
        let sections: Vec<sim::SectionIn> = app.traffic.read().await.value.as_ref()
            .and_then(|b| serde_json::from_slice(b).ok()).unwrap_or_default();
        let rain = app.weather.read().await.value.as_ref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
            .and_then(|v| v["current"]["precipitation"].as_f64()).unwrap_or(0.0) as f32;
        w.sim.write().await.set_inputs(&sim::Inputs { sections, rain });
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

/// Real bikes taken from Naolib Vélo stations become simulated cyclists leaving from there. When the operator's
/// counts stop moving (it happens: the feed can stay frozen for hours or weeks), stand-in rentals leave from
/// stations that have bikes, at a pace that follows the hour of the day.
async fn bike_departures(app: Arc<App>, w: Arc<sim::World>) {
    use rand::Rng;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let mut gone: Vec<_> = std::mem::take(&mut app.bikes.write().await.departures);
        {
            let b = app.bikes.read().await;
            if b.updated - b.reported > 1800 && !b.stations.is_empty() {
                use chrono::Timelike;
                let h = chrono::Utc::now().with_timezone(&chrono_tz::Europe::Paris).hour();
                // rentals per 5 s across the network: busy at rush hours, nearly none at night
                let rate = match h { 7..=9 | 17..=19 => 0.5, 10..=16 => 0.3, 20..=22 => 0.15, _ => 0.02 };
                let mut r = rand::thread_rng();
                if r.gen::<f64>() < rate {
                    let open: Vec<_> = b.stations.iter().filter(|s| s.renting && s.bikes > 0).collect();
                    if !open.is_empty() {
                        let s = open[r.gen_range(0..open.len())];
                        gone.push(([s.p[0] as f32, s.p[1] as f32], s.name.clone()));
                    }
                }
            }
        }
        if !gone.is_empty() {
            let mut s = w.sim.write().await;
            let ok = gone.iter().filter(|(p, name)| s.spawn_rental(*p, name)).count();
            tracing::debug!("bikes: {} Naolib rentals, {ok} cyclists sent off", gone.len());
        }
    }
}

#[derive(serde::Deserialize)]
struct ViewMsg {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

/// GET /api/world (WebSocket): the client sends {"x0","y0","x1","y1"} (world metres) whenever its view changes,
/// the server sends {"at": s, "a": [[id, kind, x, y, z, heading, speed, variant], ...]} 4 times a second for
/// what lies inside. No sign-in needed: this is the city, not the players.
async fn world_ws(up: axum::extract::ws::WebSocketUpgrade, State(w): State<Arc<sim::World>>) -> Response {
    up.max_message_size(1024).on_upgrade(move |ws| world_conn(ws, w))
}

async fn world_conn(ws: axum::extract::ws::WebSocket, w: Arc<sim::World>) {
    use axum::extract::ws::Message;
    use futures_util::{SinkExt, StreamExt};
    let (mut tx, mut rx) = ws.split();
    let mut snap = w.snap.clone();
    let mut view: Option<ViewMsg> = None;
    loop {
        tokio::select! {
            m = rx.next() => match m {
                Some(Ok(Message::Text(t))) => { if let Ok(v) = serde_json::from_str::<ViewMsg>(&t) { view = Some(v); } }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            r = snap.changed() => {
                if r.is_err() { break; }
                let Some(v) = &view else { continue };
                let s = snap.borrow_and_update().clone();
                // a view wider than ~4 km is a zoomed-out map: nothing is drawn then, send nothing
                if v.x1 - v.x0 > 4000.0 || v.y1 - v.y0 > 4000.0 { continue; }
                let mut out = String::with_capacity(16 * 1024);
                out.push_str(&format!("{{\"at\":{:.2},\"a\":[", s.at));
                let mut n = 0;
                for &(id, k, x, y, z, a, sp, c) in &s.items {
                    if x < v.x0 || x > v.x1 || y < v.y0 || y > v.y1 { continue; }
                    if n > 0 { out.push(','); }
                    out.push_str(&format!("[{id},{k},{x:.1},{y:.1},{z:.1},{a:.2},{sp:.1},{c}]"));
                    n += 1;
                    if n >= 5000 { break; }
                }
                out.push_str("]}");
                if tx.send(Message::Text(out)).await.is_err() { break; }
            }
        }
    }
}

/// GET /api/agent/:id -> who this inhabitant is and what they are doing
// ---------------------------------------------------------------- talking with the sims

#[derive(Clone)]
struct TalkState {
    app: Arc<App>,
    world: Arc<sim::World>,
    players: Arc<players::Players>,
}

#[derive(serde::Deserialize)]
struct TalkIn {
    token: String,
    id: u32,
    #[serde(default)]
    text: String,
}

/// POST /api/talk {token, id, text}: say something to inhabitant `id` (empty text: walk up to them). The player
/// must be signed in and standing next to them; they stop and face the player while the talk lasts.
async fn talk_h(State(t): State<TalkState>, axum::Json(q): axum::Json<TalkIn>) -> Response {
    let bad = |c: StatusCode, m: &str| (c, axum::Json(serde_json::json!({ "error": m }))).into_response();
    let Some(player) = t.players.name_of(&q.token) else { return bad(StatusCode::UNAUTHORIZED, "connecte-toi d'abord") };
    if t.app.talker.limited(&player) {
        return bad(StatusCode::TOO_MANY_REQUESTS, "doucement, laisse-les respirer un peu");
    }
    let pos = t.players.position_of(&player).await;
    let scene = {
        let mut s = t.world.sim.write().await;
        let Some((persona, here, doing, near)) = s.talk_context(q.id) else { return bad(StatusCode::NOT_FOUND, "personne ici") };
        if let Some((px, py)) = pos {
            if ((px as f32 - here[0]).powi(2) + (py as f32 - here[1]).powi(2)).sqrt() > 25.0 {
                return bad(StatusCode::CONFLICT, "approche-toi d'abord");
            }
        }
        let (px, py) = pos.map(|p| (p.0 as f32, p.1 as f32)).unwrap_or((here[0] + 1.0, here[1]));
        let at = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        if !s.hold(q.id, at + 90.0, px, py) {
            return bad(StatusCode::CONFLICT, "pas disponible");
        }
        let names = s.place_names(here[0], here[1]);
        let march = s.marching().map(String::from);
        (persona, doing, names, near, march)
    };
    let (persona, doing, here, near, march) = scene;
    let news: Vec<String> = t.app.news.read().await.items.iter().take(10).map(|i| format!("{} ({})", i.title, i.source)).collect();
    let events: Vec<String> = t.app.events.read().await.value.as_ref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .map(|v| upcoming(&v)).unwrap_or_default();
    let weather = t.app.weather.read().await.value.as_ref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .map(|v| format!("{:.0} °C, {}", v["current"]["temperature_2m"].as_f64().unwrap_or(12.0),
                         sky(v["current"]["weather_code"].as_i64().unwrap_or(0))))
        .unwrap_or_else(|| "inconnue".into());
    let scene = talk::Scene { persona, doing, here, near, events, news, weather, march };
    match t.app.talker.say(&player, q.id, &q.text, scene).await {
        Ok(r) => {
            if r.ended {
                let at = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
                t.world.sim.write().await.hold(q.id, at + 2.0, 0.0, 0.0);
            }
            axum::Json(r).into_response()
        }
        Err(e) => bad(StatusCode::BAD_GATEWAY, &format!("{e:#}")),
    }
}

/// POST /api/talk/end {token, id}: the player walks away; the inhabitant goes on.
async fn talk_end(State(t): State<TalkState>, axum::Json(q): axum::Json<TalkIn>) -> Response {
    if let Some(player) = t.players.name_of(&q.token) {
        t.app.talker.end(&player, q.id);
        let at = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let mut s = t.world.sim.write().await;
        if let Some((_, here, _, _)) = s.talk_context(q.id) {
            s.hold(q.id, at, here[0], here[1]);
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

/// GET /api/news: the light headlines the sims chat about, and a demonstration if one is announced today.
async fn news_h(State(t): State<TalkState>) -> Response {
    let n = t.app.news.read().await;
    axum::Json(serde_json::json!({ "items": n.items, "march": n.march, "marching": t.world.sim.read().await.marching(),
                                    "llm": t.app.talker.llm(), "updated": n.updated })).into_response()
}

/// GET /api/bikes: Naolib Vélo stations with their live bikes and free docks.
async fn bikes_h(State(t): State<TalkState>) -> Response {
    let b = t.app.bikes.read().await;
    axum::Json(serde_json::json!({ "stations": b.stations, "updated": b.updated, "reported": b.reported,
                                    "stale": b.updated - b.reported > 1800 })).into_response()
}

fn sky(code: i64) -> &'static str {
    match code {
        0 => "grand soleil",
        1..=3 => "quelques nuages",
        45 | 48 => "brouillard",
        51..=57 => "bruine",
        61..=67 | 80..=82 => "pluie",
        71..=77 | 85 | 86 => "neige",
        95..=99 => "orage",
        _ => "temps gris",
    }
}

/// The next events from the events feed, as short lines.
fn upcoming(v: &serde_json::Value) -> Vec<String> {
    let list = v.get("items").or(Some(v)).and_then(|x| x.as_array()).cloned().unwrap_or_default();
    list.iter().take(12).filter_map(|e| {
        let name = e.get("name").or(e.get("title")).and_then(|x| x.as_str())?;
        let place = e.get("place").or(e.get("lieu")).and_then(|x| x.as_str()).unwrap_or("");
        let when = e.get("date").or(e.get("start")).and_then(|x| x.as_str()).unwrap_or("");
        Some(format!("{name}{}{}", if place.is_empty() { String::new() } else { format!(", {place}") },
                     if when.is_empty() { String::new() } else { format!(" ({when})") }))
    }).collect()
}

async fn agent(State(w): State<Arc<sim::World>>, UrlPath(id): UrlPath<u32>) -> Response {
    match w.sim.read().await.describe(id) {
        Some(v) => json(Bytes::from(v.to_string())),
        None => (StatusCode::NOT_FOUND, "{}").into_response(),
    }
}
