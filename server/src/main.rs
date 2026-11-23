//! Tiny Nantes server.
//!
//! - serves the viewer and the tile pyramids (web/)
//! - keeps the city's live feeds fresh on its own timers and serves cached copies, so visitors never hit the
//!   upstream APIs: Naolib timetable (GTFS, daily) + delays (GTFS-RT, every 30 s), road traffic (every 3 min),
//!   events (hourly). When an upstream fails, the last good copy keeps being served, with its age.
//!
//! Env: TN_WEB (default ../web), TN_DATA (cache dir, default ./data), TN_PORT (default 8080), RUST_LOG.

mod feeds;
mod gtfs;
mod l93;
mod planes;
mod players;
mod rt;
mod sncf;
mod store;

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
use prost::Message;
use serde::Serialize;
use tokio::sync::RwLock;
use tower_http::compression::CompressionLayer;
use tower_http::services::ServeDir;

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
    vehicles: RwLock<(i64, Bytes)>,
    sky: Arc<RwLock<planes::Sky>>,
    sncf: RwLock<Cached<Arc<sncf::Day>>>,
    sncf_delays: RwLock<Cached<HashMap<usize, Vec<i32>>>>,
    store: Arc<store::Store>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let web = PathBuf::from(std::env::var("TN_WEB").unwrap_or_else(|_| "../web".into()));
    let data = PathBuf::from(std::env::var("TN_DATA").unwrap_or_else(|_| "data".into()));
    let port: u16 = std::env::var("TN_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);
    let origin = feeds::origin_from_world(&web.join("data/world/world.json"))?;
    let http = reqwest::Client::builder().user_agent(USER_AGENT).timeout(Duration::from_secs(120)).build()?;
    let store = Arc::new(store::Store::open(&data.join("tinynantes.db"), data.join("places"))?);
    // last good copies from the previous run: served until the first refresh succeeds
    let restore = |key: &str| {
        let mut c = Cached::<Bytes>::default();
        if let Some((ts, b)) = store.load_feed(key) {
            c.value = Some(Bytes::from(b));
            c.ok_at = Some(ts);
        }
        RwLock::new(c)
    };
    let (traffic0, events0) = (restore("traffic"), restore("events"));
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
        vehicles: RwLock::new((0, Bytes::new())),
        sky: Default::default(),
        sncf: Default::default(),
        sncf_delays: Default::default(),
    });
    let players = players::open(&app.data.join("tinynantes.db"))?;

    tokio::spawn(timetable_loop(app.clone()));
    tokio::spawn(rt_loop(app.clone()));
    tokio::spawn(every(app.clone(), 180, |a| Box::pin(refresh_traffic(a))));
    tokio::spawn(every(app.clone(), 3600, |a| Box::pin(refresh_events(a))));
    tokio::spawn(planes::run(app.http.clone(), app.origin, app.sky.clone(), store.clone()));
    tokio::spawn(places_loop(store.clone()));
    tokio::spawn(sncf_loop(app.clone()));

    let api = Router::new()
        .route("/api/transit/vehicles", get(vehicles))
        .route("/api/traffic", get(traffic))
        .route("/api/events", get(events))
        .route("/api/status", get(status))
        .route("/api/planes", get(planes_h))
        .route("/api/airport", get(airport))
        .route("/api/station", get(station))
        .route("/api/trains", get(trains))
        .route("/api/poi/:id", get(poi))
        .route("/api/places/search", get(places_search))
        .route("/api/places/menus", get(places_menus))
        .route("/healthz", get(|| async { "ok\n" }))
        .with_state(app);
    let social = Router::new()
        .route("/api/auth/login", post(players::login))
        .route("/api/ws", get(players::ws))
        .route("/api/chat/recent", get(players::recent))
        .with_state(players);
    let router = api
        .merge(social)
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
    match tokio::task::spawn_blocking(move || st.place(&id)).await.ok().flatten() {
        Some(b) => json(Bytes::from(b)),
        None => (StatusCode::NOT_FOUND, "{}").into_response(),
    }
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
