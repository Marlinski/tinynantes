//! pullrouge.fr, "pull rouge matic": a hand-kept agenda of concerts, club nights and openings in and around Nantes,
//! one long hand-written HTML page (no feed). Each entry reads
//!   samedi 13 février 2027   20h30   [*** VERNISSAGE ***]   <a href=…>Title</a>   @ venue [- CITY] / price
//! We fetch it at most every 6 hours, keep today's and tomorrow's entries, and place them on the map by matching the
//! venue against the names of the map's places (pois.json, from OpenStreetMap); entries whose venue isn't found are
//! left out (they can't be shown on the map).
use crate::feeds::{Event, Origin};
use anyhow::Result;
use chrono::NaiveDate;
use regex::Regex;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

const URL: &str = "https://www.pullrouge.fr/";
const EVERY: i64 = 6 * 3600;
const MONTHS: [&str; 12] = ["janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre",
    "octobre", "novembre", "décembre"];
/// towns on the map (an entry without a town is in Nantes)
const TOWNS: [&str; 8] = ["NANTES", "REZÉ", "REZE", "SAINT HERBLAIN", "SAINT-HERBLAIN", "BOUGUENAIS", "ORVAULT", "SAINT SÉBASTIEN"];

static PAGE: Mutex<(i64, String)> = Mutex::new((0, String::new()));
static VENUES: OnceLock<HashMap<String, [f64; 2]>> = OnceLock::new();

/// Lowercase, no accents, no articles or "salle/bar/café", no brackets: "l'Auditorium" ~ "Auditorium".
pub fn norm(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut depth = 0;
    for c in s.to_lowercase().chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            _ if depth > 0 => {}
            'à' | 'â' | 'ä' => o.push('a'),
            'é' | 'è' | 'ê' | 'ë' => o.push('e'),
            'î' | 'ï' => o.push('i'),
            'ô' | 'ö' => o.push('o'),
            'ù' | 'û' | 'ü' => o.push('u'),
            'ç' => o.push('c'),
            'œ' => o.push_str("oe"),
            c if c.is_alphanumeric() => o.push(c),
            _ => o.push(' '),
        }
    }
    const STOP: [&str; 12] = ["le", "la", "les", "l", "chez", "salle", "bar", "cafe", "the", "de", "du", "des"];
    o.split_whitespace().filter(|w| !STOP.contains(w)).collect::<Vec<_>>().join(" ")
}

fn venues(web: &Path) -> &'static HashMap<String, [f64; 2]> {
    VENUES.get_or_init(|| {
        let mut m = HashMap::new();
        let Ok(b) = std::fs::read(web.join("data/world/pois.json")) else { return m };
        let v: Vec<serde_json::Value> = serde_json::from_slice(&b).unwrap_or_default();
        for p in v {
            let (Some(n), Some(x), Some(y)) = (p["t"]["name"].as_str(), p["p"][0].as_f64(), p["p"][1].as_f64()) else { continue };
            let k = norm(n);
            if k.len() >= 3 {
                m.entry(k).or_insert([x, y]);
            }
        }
        m
    })
}

fn unescape(s: &str) -> String {
    let re = Regex::new(r"&(#x?[0-9a-fA-F]+|[a-zA-Z]+);").unwrap();
    re.replace_all(s, |c: &regex::Captures| {
        let e = &c[1];
        let ch = if let Some(h) = e.strip_prefix("#x").or(e.strip_prefix("#X")) {
            u32::from_str_radix(h, 16).ok().and_then(char::from_u32)
        } else if let Some(d) = e.strip_prefix('#') {
            d.parse().ok().and_then(char::from_u32)
        } else {
            match e {
                "nbsp" => Some(' '), "amp" => Some('&'), "lt" => Some('<'), "gt" => Some('>'), "quot" => Some('"'),
                "apos" => Some('\''), "eacute" => Some('é'), "egrave" => Some('è'), "agrave" => Some('à'),
                "ccedil" => Some('ç'), "ecirc" => Some('ê'), "rsquo" => Some('’'), "hellip" => Some('…'),
                _ => None,
            }
        };
        ch.map(String::from).unwrap_or_else(|| c[0].to_string())
    }).into_owned()
}

/// The page as text: links kept as [[url|text]], line breaks kept, everything else flattened.
fn text_of(html: &str) -> String {
    let a = Regex::new(r#"(?is)<a\b[^>]*?href="([^"]*)"[^>]*>(.*?)</a>"#).unwrap();
    let tag = Regex::new(r"(?s)<[^>]+>").unwrap();
    let s = a.replace_all(html, |c: &regex::Captures| format!(" [[{}|{}]] ", &c[1], tag.replace_all(&c[2], "")));
    let s = Regex::new(r"(?i)<br\s*/?>").unwrap().replace_all(&s, "\n");
    let s = unescape(&tag.replace_all(&s, " "));
    let s = Regex::new(r"[ \t\r\u{a0}\u{feff}]+").unwrap().replace_all(&s, " ");
    Regex::new(r" *\n *").unwrap().replace_all(&s, "\n").into_owned()
}

/// Entries of `days` from the page, with a position when the venue is a known place.
pub fn parse(html: &str, days: &[NaiveDate], o_web: &Path) -> Vec<Event> {
    let venues = venues(o_web);
    let text = text_of(html);
    let months = MONTHS.join("|");
    let day = format!(r"(?:lundi|mardi|mercredi|jeudi|vendredi|samedi|dimanche)\s+(\d{{1,2}})(?:er)?\s+({months})\s+(\d{{4}})");
    let head = Regex::new(&format!(r"{day}\s+(\d{{1,2}})h(\d{{2}})?")).unwrap();
    let link = Regex::new(r"\[\[([^|\]]*)\|([^\]]*)\]\]").unwrap();
    let kind = Regex::new(r"\*\*\*\s*(.+?)\s*\*\*\*").unwrap();
    let starts: Vec<_> = head.captures_iter(&text).map(|c| (c.get(0).unwrap().start(), c.get(0).unwrap().end(), c)).collect();
    let mut out = vec![];
    for (k, (_, end, c)) in starts.iter().enumerate() {
        let Some(month) = MONTHS.iter().position(|m| *m == &c[2]) else { continue };
        let Some(date) = NaiveDate::from_ymd_opt(c[3].parse().unwrap_or(0), month as u32 + 1, c[1].parse().unwrap_or(0)) else { continue };
        if !days.contains(&date) {
            continue;
        }
        let body_end = starts.get(k + 1).map(|s| s.0).unwrap_or(text.len());
        let body = text[*end..body_end].split_whitespace().collect::<Vec<_>>().join(" ");
        let Some(at) = body.rfind(" @ ").or_else(|| body.find('@')) else { continue };
        let (before, after) = (&body[..at], body[at..].trim_start_matches([' ', '@']).trim());
        let (venue_town, price) = after.split_once(" /").map(|(a, b)| (a.trim(), b.trim())).unwrap_or((after, ""));
        let (venue, town) = venue_town.split_once(" - ").map(|(a, b)| (a.trim(), b.trim())).unwrap_or((venue_town, ""));
        if !town.is_empty() && !TOWNS.iter().any(|t| town.to_uppercase().starts_with(t)) {
            continue;
        }
        // "Portevent > Pannonica": the venue is the last part
        let venue = venue.rsplit('>').next().unwrap_or(venue).trim();
        let Some(p) = venues.get(&norm(venue)).copied() else { continue };
        let (url, title) = match link.captures(before) {
            Some(l) => (Some(l[1].to_string()), l[2].trim().to_string()),
            None => (None, link.replace_all(before, "").to_string()),
        };
        let what = kind.captures(before).map(|m| m[1].to_string());
        let title = kind.replace_all(&title, "").trim().trim_start_matches(['.', ' ', '>']).trim().to_string();
        if title.is_empty() {
            continue;
        }
        let price = price.trim().trim_end_matches('/').trim().to_string();
        let free = Regex::new(r"(?i)gratuit|free|^0\s*€").unwrap().is_match(&price);
        let start = format!("{:02}:{}", c[4].parse::<u32>().unwrap_or(0), c.get(5).map(|m| m.as_str()).unwrap_or("00"));
        let summary = [what.clone().map(|w| w.to_lowercase()), (!price.is_empty()).then(|| price.clone())]
            .into_iter().flatten().collect::<Vec<_>>().join(" · ");
        out.push(Event {
            id: format!("pr-{}-{}-{}", date, start, norm(&title).replace(' ', "-").chars().take(40).collect::<String>()),
            name: title,
            summary,
            place: venue.to_string(),
            address: if town.is_empty() { "Nantes".into() } else { town.to_string() },
            date: date.to_string(),
            start: Some(start),
            end: None,
            types: vec![what.unwrap_or_else(|| "Concert, soirée".into())],
            themes: vec![],
            free,
            canceled: false,
            full: false,
            url,
            image: None,
            p,
            source: "pullrouge.fr".into(),
        });
    }
    out
}

/// Today's and tomorrow's entries (the page is fetched at most every 6 hours).
pub async fn events(http: &reqwest::Client, _o: Origin, web: &Path) -> Result<Vec<Event>> {
    let now = chrono::Utc::now().timestamp();
    let stale = { let p = PAGE.lock().unwrap(); now - p.0 > EVERY || p.1.is_empty() };
    if stale {
        let html = http.get(URL).send().await?.error_for_status()?.text().await?;
        *PAGE.lock().unwrap() = (now, html);
    }
    let today = crate::gtfs::today();
    let days = [today, today.succ_opt().unwrap_or(today)];
    let html = PAGE.lock().unwrap().1.clone();
    Ok(parse(&html, &days, web))
}
