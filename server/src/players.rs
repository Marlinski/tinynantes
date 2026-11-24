//! Players: names, sign-in, live positions and chat.
//!
//! - To appear on the map a visitor picks a name. A name that nobody reserved can be used as a guest
//!   (it is only locked while that guest is online); giving a password reserves it for good, and from then on
//!   the password is needed to play under that name.
//! - Positions and chat go through one WebSocket per player. Everything is logged in SQLite (sign-ins,
//!   connections, every chat message with where it was said).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::{SinkExt, StreamExt};
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

pub struct Players {
    db: Mutex<Connection>,
    online: RwLock<HashMap<u64, Player>>,
    tx: broadcast::Sender<Arc<str>>,
    next_id: AtomicU64,
    failures: Mutex<HashMap<String, Vec<Instant>>>,
}

#[derive(Clone, Serialize)]
pub struct Player {
    pub id: u64,
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    /// heading, radians (world frame)
    pub a: f64,
    /// walk | run | bike | car | tram
    pub m: String,
    #[serde(skip)]
    pub seen: Option<Instant>,
}

impl Players {
    /// The name behind a session token.
    pub fn name_of(&self, token: &str) -> Option<String> {
        self.db.lock().unwrap()
            .query_row("SELECT name FROM sessions WHERE token = ?1", params![token], |r| r.get(0))
            .optional().unwrap_or(None)
    }
    /// Where a player who is online stands (world metres).
    pub async fn position_of(&self, name: &str) -> Option<(f64, f64)> {
        self.online.read().await.values().find(|q| q.name == name).map(|q| (q.x, q.y))
    }
}

pub fn open(path: &std::path::Path) -> Result<Arc<Players>> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let db = Connection::open(path)?;
    db.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS users (key TEXT PRIMARY KEY, name TEXT NOT NULL, hash TEXT NOT NULL, created INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS sessions (token TEXT PRIMARY KEY, name TEXT NOT NULL, reserved INTEGER NOT NULL, created INTEGER NOT NULL, ip TEXT);
         CREATE TABLE IF NOT EXISTS chat (id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, name TEXT NOT NULL, x REAL, y REAL, text TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS log (id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, kind TEXT NOT NULL, name TEXT, ip TEXT, detail TEXT);",
    )?;
    let (tx, _) = broadcast::channel(1024);
    let p = Arc::new(Players {
        db: Mutex::new(db),
        online: RwLock::new(HashMap::new()),
        tx,
        next_id: AtomicU64::new(1),
        failures: Mutex::new(HashMap::new()),
    });
    tokio::spawn(tick(p.clone()));
    Ok(p)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl Players {
    fn log(&self, kind: &str, name: Option<&str>, ip: Option<&str>, detail: &str) {
        tracing::info!(kind, name, ip, detail, "player");
        if let Ok(db) = self.db.lock() {
            let _ = db.execute(
                "INSERT INTO log (ts, kind, name, ip, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![now(), kind, name, ip, detail],
            );
        }
    }
}

/// Names: 3 to 20 letters, digits, '-', '_' or '.', starting with a letter or digit.
fn valid_name(n: &str) -> bool {
    let c = n.chars().count();
    (3..=20).contains(&c)
        && n.chars().all(|ch| ch.is_alphanumeric() || "-_.".contains(ch))
        && n.chars().next().map(|ch| ch.is_alphanumeric()).unwrap_or(false)
}
fn key(n: &str) -> String {
    n.to_lowercase()
}

fn client_ip(headers: &HeaderMap, addr: SocketAddr) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| addr.ip().to_string())
}

#[derive(Deserialize)]
pub struct Login {
    name: String,
    password: Option<String>,
}

#[derive(Serialize)]
struct LoginOk {
    token: String,
    name: String,
    reserved: bool,
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(serde_json::json!({ "error": msg }))).into_response()
}

/// POST /api/auth/login {name, password?}
pub async fn login(
    State(p): State<Arc<Players>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<Login>,
) -> Response {
    let ip = client_ip(&headers, addr);
    let name = req.name.trim().to_string();
    if !valid_name(&name) {
        return err(StatusCode::BAD_REQUEST, "Nom : 3 à 20 lettres, chiffres, - _ ou .");
    }
    let k = key(&name);
    {
        let mut f = p.failures.lock().unwrap();
        let v = f.entry(k.clone()).or_default();
        v.retain(|t| t.elapsed() < Duration::from_secs(600));
        if v.len() >= 6 {
            return err(StatusCode::TOO_MANY_REQUESTS, "Trop d'essais, réessayez dans quelques minutes");
        }
    }
    let password = req.password.filter(|s| !s.is_empty());
    let existing: Option<(String, String)> = {
        let db = p.db.lock().unwrap();
        db.query_row("SELECT name, hash FROM users WHERE key = ?1", params![k], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .unwrap_or(None)
    };
    let (shown, reserved) = match (existing, password) {
        (Some((shown, hash)), Some(pw)) => {
            let ok = PasswordHash::new(&hash).map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok()).unwrap_or(false);
            if !ok {
                p.failures.lock().unwrap().entry(k.clone()).or_default().push(Instant::now());
                p.log("login_failed", Some(&name), Some(&ip), "");
                return err(StatusCode::UNAUTHORIZED, "Mot de passe incorrect");
            }
            (shown, true)
        }
        (Some(_), None) => return err(StatusCode::CONFLICT, "Ce nom est réservé : mot de passe requis"),
        (None, Some(pw)) => {
            if pw.chars().count() < 6 {
                return err(StatusCode::BAD_REQUEST, "Mot de passe : 6 caractères minimum");
            }
            let salt = SaltString::generate(&mut OsRng);
            let Ok(hash) = Argon2::default().hash_password(pw.as_bytes(), &salt) else {
                return err(StatusCode::INTERNAL_SERVER_ERROR, "hash");
            };
            let r = p.db.lock().unwrap().execute(
                "INSERT INTO users (key, name, hash, created) VALUES (?1, ?2, ?3, ?4)",
                params![k, name, hash.to_string(), now()],
            );
            if r.is_err() {
                return err(StatusCode::CONFLICT, "Ce nom vient d'être réservé");
            }
            p.log("register", Some(&name), Some(&ip), "");
            (name.clone(), true)
        }
        (None, None) => {
            let taken = p.online.read().await.values().any(|q| key(&q.name) == k);
            if taken {
                return err(StatusCode::CONFLICT, "Ce nom est déjà utilisé en ce moment");
            }
            (name.clone(), false)
        }
    };
    let mut b = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut b);
    let token: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let _ = p.db.lock().unwrap().execute(
        "INSERT INTO sessions (token, name, reserved, created, ip) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![token, shown, reserved as i32, now(), ip],
    );
    p.log("login", Some(&shown), Some(&ip), if reserved { "reserved" } else { "guest" });
    Json(LoginOk { token, name: shown, reserved }).into_response()
}

#[derive(Deserialize)]
pub struct WsQuery {
    token: String,
}

/// GET /api/ws?token=...
pub async fn ws(
    State(p): State<Arc<Players>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<WsQuery>,
    up: WebSocketUpgrade,
) -> Response {
    let name: Option<String> = p
        .db
        .lock()
        .unwrap()
        .query_row("SELECT name FROM sessions WHERE token = ?1", params![q.token], |r| r.get(0))
        .optional()
        .unwrap_or(None);
    let Some(name) = name else { return err(StatusCode::UNAUTHORIZED, "session inconnue") };
    let ip = client_ip(&headers, addr);
    up.max_message_size(4096).on_upgrade(move |sock| session(p, sock, name, ip))
}

#[derive(Deserialize)]
#[serde(tag = "t")]
enum In {
    #[serde(rename = "pos")]
    Pos { x: f64, y: f64, z: f64, a: f64, m: String },
    #[serde(rename = "chat")]
    Chat { text: String },
}

async fn session(p: Arc<Players>, sock: WebSocket, name: String, ip: String) {
    let id = p.next_id.fetch_add(1, Ordering::Relaxed);
    p.log("join", Some(&name), Some(&ip), &id.to_string());
    let (mut tx, mut rx) = sock.split();
    let mut sub = p.tx.subscribe();
    let hello = serde_json::json!({"t": "hello", "id": id, "name": name}).to_string();
    if tx.send(Message::Text(hello)).await.is_err() {
        return;
    }
    let mut last_chat = Instant::now() - Duration::from_secs(10);
    let mut pos_budget = (Instant::now(), 0u32);
    loop {
        tokio::select! {
            m = rx.next() => {
                let Some(Ok(m)) = m else { break };
                let Message::Text(t) = m else { continue };
                let Ok(msg) = serde_json::from_str::<In>(&t) else { continue };
                match msg {
                    In::Pos { x, y, z, a, m } => {
                        if pos_budget.0.elapsed() > Duration::from_secs(1) { pos_budget = (Instant::now(), 0); }
                        pos_budget.1 += 1;
                        if pos_budget.1 > 15 || !(x.is_finite() && y.is_finite() && z.is_finite() && a.is_finite()) { continue; }
                        let m: String = m.chars().take(8).collect();
                        p.online.write().await.insert(id, Player { id, name: name.clone(), x, y, z, a, m, seen: Some(Instant::now()) });
                    }
                    In::Chat { text } => {
                        let text: String = text.trim().chars().filter(|c| !c.is_control()).take(280).collect();
                        if text.is_empty() || last_chat.elapsed() < Duration::from_millis(800) { continue; }
                        last_chat = Instant::now();
                        let (x, y) = p.online.read().await.get(&id).map(|q| (q.x, q.y)).unwrap_or((f64::NAN, f64::NAN));
                        let ts = now();
                        let _ = p.db.lock().unwrap().execute(
                            "INSERT INTO chat (ts, name, x, y, text) VALUES (?1, ?2, ?3, ?4, ?5)",
                            params![ts, name, x, y, text],
                        );
                        tracing::info!(name, text, "chat");
                        let out = serde_json::json!({"t": "chat", "id": id, "name": name, "text": text, "x": x, "y": y, "ts": ts});
                        let _ = p.tx.send(out.to_string().into());
                    }
                }
            }
            b = sub.recv() => {
                match b {
                    Ok(s) => if tx.send(Message::Text(s.to_string())).await.is_err() { break },
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        }
    }
    p.online.write().await.remove(&id);
    let _ = p.tx.send(serde_json::json!({"t": "leave", "id": id}).to_string().into());
    p.log("leave", Some(&name), Some(&ip), &id.to_string());
}

/// Every 200 ms: everyone's position to everyone (fine for a few hundred players).
async fn tick(p: Arc<Players>) {
    let mut iv = tokio::time::interval(Duration::from_millis(200));
    loop {
        iv.tick().await;
        let mut on = p.online.write().await;
        on.retain(|_, q| q.seen.map(|s| s.elapsed() < Duration::from_secs(30)).unwrap_or(false));
        if on.is_empty() || p.tx.receiver_count() == 0 {
            continue;
        }
        let list: Vec<&Player> = on.values().collect();
        let _ = p.tx.send(serde_json::json!({"t": "players", "list": list}).to_string().into());
    }
}

/// GET /api/chat/recent: the last messages (for a newcomer's chat panel).
pub async fn recent(State(p): State<Arc<Players>>) -> Response {
    let db = p.db.lock().unwrap();
    let mut st = match db.prepare("SELECT ts, name, text FROM chat ORDER BY id DESC LIMIT 40") {
        Ok(s) => s,
        Err(_) => return err(StatusCode::INTERNAL_SERVER_ERROR, "db"),
    };
    let rows: Vec<serde_json::Value> = st
        .query_map([], |r| Ok(serde_json::json!({"ts": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "text": r.get::<_, String>(2)?})))
        .map(|it| it.filter_map(|x| x.ok()).collect())
        .unwrap_or_default();
    Json(rows.into_iter().rev().collect::<Vec<_>>()).into_response()
}
