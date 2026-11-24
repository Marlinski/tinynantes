//! Local news for the sims to chat about: headlines from a few Nantes RSS feeds, every 30 minutes. Anything
//! tragic (deaths, crimes, accidents, trials...) is dropped by keyword before it gets near a conversation; the
//! sims' instructions also tell them to stay light. Also spots announced demonstrations, so a march can walk
//! through the centre on the day.

use std::sync::Arc;

use chrono::{DateTime, Datelike, Utc};
use regex::Regex;
use serde::Serialize;
use tokio::sync::RwLock;

const FEEDS: &[(&str, &str)] = &[
    ("20 Minutes Nantes", "https://www.20minutes.fr/feeds/rss-nantes.xml"),
    ("Ouest-France Nantes", "https://www.ouest-france.fr/rss-en-continu.xml?ville=nantes"),
    ("France 3 Nantes", "https://france3-regions.francetvinfo.fr/pays-de-la-loire/loire-atlantique/nantes/rss"),
];

/// Words that make a headline unsuitable for small talk.
const GRIM: &str = r"(?i)\b(mort|morte|morts|décès|décéd|tué|tuée|meurtr|homicide|assassin|agress|viol|violée|violence|accident|blessé|incendie|drame|tragique|suicide|procès|condamn|prison|couteau|poignard|fusillade|arme|disparu|disparition|noyade|noyé|attentat|terror|guerre|crash|collision|victime|enquête|garde à vue|interpell|trafic|drogue|stupéfiant|cambriol|braquage|harcèlement|pédo|abus|feu|explosion|deuil|hommage|obsèques|cancer|maladie|licenci|faillite|liquidation|fermeture|polémique|plainte|sexuel|raciste|antisémit|islamophob|haine|insulte|menace|rixe|bagarre|émeute|police|justice|juge|tribunal|rétention|intégris|extrême|expuls|migrant|sans-papiers|préfecture|grève|crise)";
const MARCH: &str = r"(?i)\b(manifestation|manif|manifestants|cortège|rassemblement|défilé|marche pour|marche contre|mobilisation|grève)\b";

#[derive(Clone, Serialize)]
pub struct Item {
    pub source: String,
    pub title: String,
    pub summary: String,
    pub link: String,
    pub at: i64,
}

#[derive(Default)]
pub struct News {
    pub items: Vec<Item>,
    /// a demonstration announced for today, and the headline it comes from
    pub march: Option<String>,
    pub updated: i64,
}

fn text(item: &str, tag: &str) -> String {
    let re = Regex::new(&format!(r"(?s)<{tag}[^>]*>(.*?)</{tag}>")).unwrap();
    let raw = re.captures(item).map(|c| c[1].to_string()).unwrap_or_default();
    let raw = raw.trim().trim_start_matches("<![CDATA[").trim_end_matches("]]>");
    let tags = Regex::new(r"<[^>]+>").unwrap();
    let s = tags.replace_all(raw, " ");
    s.replace("&amp;", "&").replace("&quot;", "\"").replace("&#039;", "'").replace("&#39;", "'").replace("&apos;", "'")
        .replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ").replace("&rsquo;", "’")
        .split_whitespace().collect::<Vec<_>>().join(" ")
}

async fn fetch(http: &reqwest::Client) -> Vec<Item> {
    let item_re = Regex::new(r"(?s)<item[ >].*?</item>").unwrap();
    let mut out = vec![];
    for (source, url) in FEEDS {
        let Ok(r) = http.get(*url).send().await else { continue };
        let Ok(body) = r.text().await else { continue };
        for m in item_re.find_iter(&body).take(40) {
            let it = m.as_str();
            let at = DateTime::parse_from_rfc2822(&text(it, "pubDate")).map(|d| d.timestamp()).unwrap_or(0);
            let mut summary = text(it, "description");
            if summary.chars().count() > 300 {
                summary = summary.chars().take(300).collect::<String>() + "…";
            }
            out.push(Item { source: source.to_string(), title: text(it, "title"), summary, link: text(it, "link"), at });
        }
    }
    out
}

/// Keep the light headlines (not older than 4 days), newest first, one per title.
fn light(items: &[Item], now: i64) -> Vec<Item> {
    let grim = Regex::new(GRIM).unwrap();
    let mut seen = std::collections::HashSet::new();
    let mut v: Vec<Item> = items.iter()
        .filter(|i| now - i.at < 4 * 86400 && !i.title.is_empty())
        .filter(|i| !grim.is_match(&i.title) && !grim.is_match(&i.summary))
        .filter(|i| seen.insert(i.title.to_lowercase()))
        .cloned().collect();
    v.sort_by_key(|i| -i.at);
    v.truncate(30);
    v
}

/// A demonstration in Nantes announced for today: a recent headline about one that names today (or
/// "aujourd'hui", "ce samedi"...). Grim ones (clashes) don't count.
fn march_today(items: &[Item], now: DateTime<Utc>) -> Option<String> {
    let m = Regex::new(MARCH).unwrap();
    let grim = Regex::new(GRIM).unwrap();
    let paris = now.with_timezone(&chrono_tz::Europe::Paris);
    let day = ["lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche"][paris.weekday().num_days_from_monday() as usize];
    let today = Regex::new(&format!(r"(?i)\b(aujourd'hui|aujourd’hui|ce {day}|{day} {}|cet après-midi)\b", paris.day())).unwrap();
    items.iter()
        .filter(|i| now.timestamp() - i.at < 36 * 3600)
        .find(|i| {
            let t = format!("{} {}", i.title, i.summary);
            m.is_match(&t) && !grim.is_match(&i.title) && (today.is_match(&t) || now.timestamp() - i.at < 6 * 3600)
        })
        .map(|i| i.title.clone())
}

pub async fn run(http: reqwest::Client, news: Arc<RwLock<News>>) {
    loop {
        let raw = fetch(&http).await;
        let now = Utc::now();
        if !raw.is_empty() {
            let mut n = news.write().await;
            n.march = march_today(&raw, now);
            n.items = light(&raw, now.timestamp());
            n.updated = now.timestamp();
            tracing::info!("news: {} light headlines of {}, march: {:?}", n.items.len(), raw.len(), n.march);
        }
        tokio::time::sleep(std::time::Duration::from_secs(1800)).await;
    }
}
