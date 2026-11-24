//! Prometheus metrics (text exposition format), for Grafana. Served on their own port (TN_METRICS_PORT, default
//! 9100; 0 = off), which is meant to stay inside the cluster: it is not behind the public site.
//!
//! Counters are bumped where things happen (`inc`); gauges are read at scrape time by main.rs (`gauge`).
use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::Mutex;

/// (name, labels as `k="v",...`) -> value
static COUNTERS: Mutex<BTreeMap<(&'static str, String), f64>> = Mutex::new(BTreeMap::new());

/// What each metric means (HELP) and its type, printed once per name.
const HELP: &[(&str, &str, &str)] = &[
    ("tn_http_requests_total", "counter", "HTTP requests, by route group and status class"),
    ("tn_http_request_seconds_total", "counter", "Time spent answering HTTP requests, by route group"),
    ("tn_logins_total", "counter", "Sign-in attempts, by outcome (ok, new_account, bad_password, throttled)"),
    ("tn_ws_sessions_total", "counter", "Live sessions opened (WebSocket), players and spectators"),
    ("tn_chat_messages_total", "counter", "Chat messages posted"),
    ("tn_talk_turns_total", "counter", "Turns of conversations with inhabitants, by outcome (llm, template, walk_off_regex, walk_off_classifier, cold_shoulder)"),
    ("tn_talk_conversations_total", "counter", "Conversations started with an inhabitant"),
    ("tn_talk_ended_total", "counter", "Conversations the inhabitant ended (said goodbye or walked off)"),
    ("tn_llm_requests_total", "counter", "Language model calls, by kind (reply, guard) and model that answered"),
    ("tn_llm_errors_total", "counter", "Language model calls that failed, by kind"),
    ("tn_llm_tokens_total", "counter", "Language model tokens, by kind and direction (in, out)"),
    ("tn_crashes_total", "counter", "Players' cars blown up, by what they hit (wall, vehicle)"),
    ("tn_knockdowns_total", "counter", "Inhabitants run over by players"),
    ("tn_carjacks_total", "counter", "Cars taken from inhabitants by players"),
    ("tn_impacts_refused_total", "counter", "Collision reports from players the server found implausible, by kind"),
    ("tn_service_dispatches_total", "counter", "Service vehicles sent to a scene, by service"),
    ("tn_sim_reroutes_total", "counter", "Sim cars that turned round out of a jam to find another way"),
    ("tn_debug_actions_total", "counter", "Debug panel actions accepted, by action"),
    ("tn_debug_denied_total", "counter", "Debug panel actions refused (no or wrong admin key)"),
    ("tn_build_info", "gauge", "Build version"),
    ("tn_uptime_seconds", "gauge", "Seconds since the server started"),
    ("tn_process_resident_bytes", "gauge", "Resident memory of the server"),
    ("tn_players_online", "gauge", "Players walking around the map right now"),
    ("tn_users_registered", "gauge", "Names reserved with a password (players can also play as guests)"),
    ("tn_users_active", "gauge", "Distinct player names (guests included) who signed in during the window"),
    ("tn_talk_players", "gauge", "Distinct players who talked with an inhabitant in the window"),
    ("tn_talk_messages", "gauge", "Conversation lines logged in the window, by role (player, sim, guard)"),
    ("tn_llm_replies_today", "gauge", "Inhabitants' replies written by the model today (UTC)"),
    ("tn_llm_daily_cap", "gauge", "Model replies allowed per day"),
    ("tn_sim_agents", "gauge", "Simulated inhabitants on the map, by kind"),
    ("tn_sim_march_active", "gauge", "1 while a demonstration is under way"),
    ("tn_feed_age_seconds", "gauge", "Seconds since a live feed last refreshed successfully"),
    ("tn_feed_up", "gauge", "1 if a live feed's last refresh succeeded"),
    ("tn_planes", "gauge", "Aircraft seen around the airport"),
    ("tn_bike_stations", "gauge", "Naolib Vélo stations, by state (renting, out_of_service)"),
    ("tn_bikes_available", "gauge", "Naolib Vélo bikes available at stations"),
    ("tn_events_today", "gauge", "Events listed for today and tomorrow, by source"),
    ("tn_news_items", "gauge", "Local news headlines the inhabitants know about"),
];

fn labels(l: &[(&str, &str)]) -> String {
    let mut s = String::new();
    for (k, v) in l {
        if !s.is_empty() {
            s.push(',');
        }
        let v = v.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', " ");
        let _ = write!(s, "{k}=\"{v}\"");
    }
    s
}

/// Add `v` to counter `name{labels}`.
pub fn inc(name: &'static str, l: &[(&str, &str)], v: f64) {
    *COUNTERS.lock().unwrap().entry((name, labels(l))).or_insert(0.0) += v;
}

/// Gauges read at scrape time, written next to the counters.
#[derive(Default)]
pub struct Page {
    gauges: BTreeMap<(&'static str, String), f64>,
}

impl Page {
    pub fn gauge(&mut self, name: &'static str, l: &[(&str, &str)], v: f64) {
        self.gauges.insert((name, labels(l)), v);
    }

    pub fn render(self) -> String {
        let mut all: BTreeMap<&'static str, Vec<(String, f64)>> = BTreeMap::new();
        for ((n, l), v) in COUNTERS.lock().unwrap().iter() {
            all.entry(n).or_default().push((l.clone(), *v));
        }
        for ((n, l), v) in self.gauges {
            all.entry(n).or_default().push((l, v));
        }
        let mut out = String::new();
        for (name, kind, help) in HELP {
            let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} {kind}");
            for (l, v) in all.get(name).map(|v| v.as_slice()).unwrap_or(&[]) {
                if l.is_empty() {
                    let _ = writeln!(out, "{name} {v}");
                } else {
                    let _ = writeln!(out, "{name}{{{l}}} {v}");
                }
            }
        }
        out
    }
}

/// Resident memory (Linux), bytes.
pub fn rss() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: f64 = s.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096.0)
}

/// Group a path for the request counter (bounded cardinality: never the raw path).
pub fn route(path: &str) -> &'static str {
    const API: &[&str] = &["agent", "airport", "auth", "bikes", "chat", "debug", "events", "impact", "news", "places", "planes", "poi",
        "station", "status", "talk", "traffic", "trains", "transit", "vehicles", "weather", "world", "ws"];
    if let Some(rest) = path.strip_prefix("/api/") {
        let first = rest.split('/').next().unwrap_or("");
        return API.iter().find(|a| **a == first).copied().unwrap_or("api_other");
    }
    if path.starts_with("/data/h") {
        "tiles"
    } else if path.starts_with("/data/") {
        "data"
    } else if path == "/healthz" {
        "healthz"
    } else {
        "static"
    }
}
