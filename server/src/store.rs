//! SQLite store (TN_DATA/tinynantes.db, shared with players.rs; WAL mode):
//! - `feed_cache`: last good copy of every upstream feed, so a restart serves data immediately and an
//!   upstream outage never empties the map;
//! - `routes`: adsbdb flight routes per callsign (looked up once, kept for days);
//! - `places` + `places_fts`: what menus.rs read on each place's website (menu, pictures, links), queryable by id
//!   and by full-text search (name, menu items); seeded from TN_PLACES/*.json (written by `menus-crawl`, shipped
//!   in the image), then kept fresh lazily and nightly. Also how often each place is opened.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

pub struct Store {
    db: Mutex<Connection>,
    places_dir: PathBuf,
}

impl Store {
    pub fn open(path: &Path, places_dir: PathBuf) -> Result<Store> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let db = Connection::open(path)?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS feed_cache (key TEXT PRIMARY KEY, ts INTEGER NOT NULL, body BLOB NOT NULL);
             CREATE TABLE IF NOT EXISTS routes (callsign TEXT PRIMARY KEY, ts INTEGER NOT NULL, route TEXT);
             CREATE TABLE IF NOT EXISTS places (id TEXT PRIMARY KEY, name TEXT, kind TEXT, has_menu INTEGER NOT NULL,
                                                fetched INTEGER, mtime INTEGER NOT NULL, body TEXT NOT NULL);
             CREATE VIRTUAL TABLE IF NOT EXISTS places_fts USING fts5(id UNINDEXED, name, text,
                                                tokenize = 'unicode61 remove_diacritics 2');",
        )?;
        // added later: how often and when people open a place (the nightly refresh follows them)
        let _ = db.execute("ALTER TABLE places ADD COLUMN views INTEGER NOT NULL DEFAULT 0", []);
        let _ = db.execute("ALTER TABLE places ADD COLUMN last_view INTEGER", []);
        Ok(Store { db: Mutex::new(db), places_dir })
    }

    // ------------------------------------------------------------ feeds
    pub fn save_feed(&self, key: &str, ts: i64, body: &[u8]) {
        let db = self.db.lock().unwrap();
        if let Err(e) = db.execute("INSERT OR REPLACE INTO feed_cache (key, ts, body) VALUES (?1, ?2, ?3)", params![key, ts, body]) {
            tracing::warn!("store {key}: {e}");
        }
    }
    pub fn load_feed(&self, key: &str) -> Option<(i64, Vec<u8>)> {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT ts, body FROM feed_cache WHERE key = ?1", [key], |r| Ok((r.get(0)?, r.get(1)?))).optional().ok().flatten()
    }

    // ------------------------------------------------------------ flight routes
    /// (looked up at, route JSON or None when adsbdb doesn't know the callsign)
    pub fn route(&self, callsign: &str) -> Option<(i64, Option<String>)> {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT ts, route FROM routes WHERE callsign = ?1", [callsign], |r| Ok((r.get(0)?, r.get(1)?))).optional().ok().flatten()
    }
    pub fn save_route(&self, callsign: &str, ts: i64, route: Option<&str>) {
        let db = self.db.lock().unwrap();
        let _ = db.execute("INSERT OR REPLACE INTO routes (callsign, ts, route) VALUES (?1, ?2, ?3)", params![callsign, ts, route]);
    }

    // ------------------------------------------------------------ places
    /// Import crawler files that are new or changed since the last import. Returns how many were imported.
    pub fn import_places(&self) -> Result<usize> {
        let Ok(rd) = std::fs::read_dir(&self.places_dir) else { return Ok(0) };
        let mut db = self.db.lock().unwrap();
        let known: std::collections::HashMap<String, (i64, Option<i64>)> = {
            let mut st = db.prepare("SELECT id, mtime, fetched FROM places")?;
            let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)?, r.get::<_, Option<i64>>(2)?))))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        let tx = db.transaction()?;
        let mut n = 0;
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let id = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let mtime = e.metadata().ok().and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0);
            let prev = known.get(&id);
            if prev.map(|p| p.0) == Some(mtime) {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&p) else { continue };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else { continue };
            if prev.and_then(|p| p.1).unwrap_or(0) > v["fetched"].as_i64().unwrap_or(0) {
                // the server read the site more recently than the crawler file
                tx.execute("UPDATE places SET mtime = ?2 WHERE id = ?1", params![id, mtime])?;
                continue;
            }
            upsert(&tx, &id, &v, mtime)?;
            n += 1;
        }
        tx.commit()?;
        Ok(n)
    }

    /// Save what the server just read from a place's website. Keeps the previous menu when the site was down.
    pub fn save_place(&self, id: &str, v: &mut serde_json::Value) {
        let mut db = self.db.lock().unwrap();
        if v.get("error").is_some() {
            let old: Option<String> = db.query_row("SELECT body FROM places WHERE id = ?1", [id], |r| r.get(0)).optional().ok().flatten();
            if let Some(o) = old.and_then(|o| serde_json::from_str::<serde_json::Value>(&o).ok()) {
                for k in ["sections", "images", "pdfs", "links", "menu_url", "source", "description", "image"] {
                    if !o[k].is_null() {
                        v[k] = o[k].clone();
                    }
                }
            }
        }
        let r = db.transaction().map_err(anyhow::Error::from).and_then(|tx| {
            upsert(&tx, id, v, 0)?;
            tx.commit()?;
            Ok(())
        });
        if let Err(e) = r {
            tracing::warn!("save place {id}: {e:#}");
        }
    }

    /// (body, when the website was read) and count a view
    pub fn open_place(&self, id: &str, now: i64) -> Option<(String, Option<i64>)> {
        let db = self.db.lock().unwrap();
        let _ = db.execute("UPDATE places SET views = views + 1, last_view = ?2 WHERE id = ?1", params![id, now]);
        db.query_row("SELECT body, fetched FROM places WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional().ok().flatten()
    }

    /// places opened since `since`, most viewed first (the nightly refresh)
    pub fn viewed_since(&self, since: i64) -> Vec<String> {
        let db = self.db.lock().unwrap();
        let Ok(mut st) = db.prepare("SELECT id FROM places WHERE last_view >= ?1 ORDER BY views DESC") else { return vec![] };
        st.query_map([since], |r| r.get(0)).map(|it| it.filter_map(|r| r.ok()).collect()).unwrap_or_default()
    }

    /// Full-text search over names and menus: [(id, name, kind, has_menu, snippet)]
    pub fn search(&self, q: &str, limit: usize) -> Vec<serde_json::Value> {
        // keep only words, each as a prefix query: "crêpe sarrasin" -> "crêpe"* "sarrasin"*
        let terms: Vec<String> = q.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).take(6)
            .map(|w| format!("\"{w}\"*")).collect();
        if terms.is_empty() {
            return vec![];
        }
        let db = self.db.lock().unwrap();
        let Ok(mut st) = db.prepare(
            "SELECT f.id, p.name, p.kind, p.has_menu, snippet(places_fts, 2, '[', ']', '…', 8)
             FROM places_fts f JOIN places p ON p.id = f.id WHERE places_fts MATCH ?1 ORDER BY rank LIMIT ?2",
        ) else { return vec![] };
        let rows = st.query_map(params![terms.join(" "), limit as i64], |r| {
            Ok(serde_json::json!({"id": r.get::<_, String>(0)?, "name": r.get::<_, Option<String>>(1)?,
                "kind": r.get::<_, Option<String>>(2)?, "menu": r.get::<_, bool>(3)?, "match": r.get::<_, String>(4)?}))
        });
        rows.map(|it| it.filter_map(|r| r.ok()).collect()).unwrap_or_default()
    }

    pub fn place_ids_with_menu(&self) -> Vec<String> {
        let db = self.db.lock().unwrap();
        let Ok(mut st) = db.prepare("SELECT id FROM places WHERE has_menu = 1") else { return vec![] };
        st.query_map([], |r| r.get(0)).map(|it| it.filter_map(|r| r.ok()).collect()).unwrap_or_default()
    }
}

/// Insert or update a place (keeping its view counts) and its search entry.
fn upsert(tx: &rusqlite::Transaction, id: &str, v: &serde_json::Value, mtime: i64) -> Result<()> {
    let name = v["name"].as_str().unwrap_or("");
    let mut text = String::new();
    for s in v["sections"].as_array().into_iter().flatten() {
        text.push_str(s["title"].as_str().unwrap_or(""));
        text.push('\n');
        for it in s["items"].as_array().into_iter().flatten() {
            text.push_str(it["name"].as_str().unwrap_or(""));
            text.push('\n');
            text.push_str(it["desc"].as_str().unwrap_or(""));
            text.push('\n');
        }
    }
    if let Some(d) = v["description"].as_str() {
        text.push_str(d);
    }
    let nonempty = |k: &str| v[k].as_array().is_some_and(|a| !a.is_empty());
    let has_menu = nonempty("sections") || nonempty("images") || nonempty("pdfs");
    tx.execute(
        "INSERT INTO places (id, name, kind, has_menu, fetched, mtime, body) VALUES (?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, kind = excluded.kind, has_menu = excluded.has_menu,
           fetched = excluded.fetched, mtime = CASE WHEN excluded.mtime > 0 THEN excluded.mtime ELSE places.mtime END,
           body = excluded.body",
        params![id, name, v["kind"].as_str(), has_menu, v["fetched"].as_i64(), mtime, v.to_string()],
    )?;
    tx.execute("DELETE FROM places_fts WHERE id = ?1", [id])?;
    tx.execute("INSERT INTO places_fts (id, name, text) VALUES (?1, ?2, ?3)", params![id, name, text])?;
    Ok(())
}
