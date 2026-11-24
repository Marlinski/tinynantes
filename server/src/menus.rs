//! Menus of places (restaurants, cafés, bars…), read from their own websites, lazily.
//!
//! - Fetching is polite: robots.txt honoured (for `TinyNantesBot` and `*`), one request per second per host,
//!   pages capped at 8 MB, an identifying User-Agent.
//! - A place is (re)read when someone opens it and its copy is older than a day ([`STALE`]), and every night for
//!   the places people looked at recently (main.rs). Results go to the SQLite store.
//! - Extraction runs plugins in order and keeps the richest result:
//!   1. `jsonld`: schema.org `Menu` / `MenuSection` / `MenuItem` (the clean case);
//!   2. `text`: the page as lines of text (block by block), dishes paired with prices even when the price sits on
//!      its own line after the name and description (Wix, WordPress themes, dish.co…), sections from headings
//!      and short capitalised lines;
//!   3. `pdf`: menus published as PDF, same line pairing on the extracted text.
//!   Besides, every page is scanned for menu images (shown as such), PDF menus, and links to delivery and booking
//!   platforms (Uber Eats, Deliveroo, Just Eat, TheFork, Zenchef…): their pages are not read (their terms or
//!   robots.txt forbid it), the panel links to them.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use regex::Regex;
use scraper::{ElementRef, Html, Node};
use serde_json::{json, Value};
use url::Url;

pub const STALE: i64 = 24 * 3600;
const UA: &str = "TinyNantesBot/0.2 (+https://tinynantes.fr; map of Nantes, shows menus with a link to the source)";
const MAX_BYTES: usize = 8 << 20;
const MIN_ITEMS: usize = 4;

static PRICE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:(\d{1,3}(?:\s?[.,]\s?\d{1,2})?)\s?(?:€|eur\b|euros?\b))|(?:€\s?(\d{1,3}(?:\s?[.,]\s?\d{1,2})?))").unwrap()
});
/// a line that is only a price (or two: "6 € / 11 €"), possibly without the € sign when it has decimals ("7,50")
static PRICE_ONLY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^[\s(]*(?:€\s?)?(\d{1,3}(?:\s?[.,]\s?\d{1,2})?)\s?(?:€|eur|euros?)?\s*(?:[/|-]\s*.*)?$").unwrap()
});
static DECIMALS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d[.,]\d{1,2}").unwrap());
static WORDY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\W\d_]{3}").unwrap());
static MENU_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(la[-_ ]?carte|\bcarte\b|menus?\b|nos[-_ ]plats|formules?|ardoise|\bplats\b|\bfood\b|drinks?|boissons|cocktails|\bvins?\b|tapas|pizzas?|burgers?|desserts?|brunch|midi|d[ée]jeuner|d[îi]ner)").unwrap()
});
static JUNK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(livraison|frais de|minimum|carte cadeau|bon cadeau|ch[èe]que|ticket|commande|panier|r[ée]duction|offert|code promo|newsletter|cookies?|©|^\W*(add|ajouter|commander|voir|choisir|options?|continuer)\b|add to cart|produit à voir|en savoir plus|lire la suite|d[ée]couvrir)").unwrap()
});
const SKIP: &[&str] = &["script", "style", "noscript", "svg", "template", "iframe", "button", "select", "nav", "footer", "form", "head"];
const BLOCK: &[&str] = &["p", "div", "li", "ul", "ol", "tr", "td", "th", "table", "tbody", "thead", "section", "article", "main",
    "aside", "header", "dd", "dt", "dl", "blockquote", "figure", "figcaption", "br", "hr", "h1", "h2", "h3", "h4", "h5", "h6",
    "body", "html", "pre", "address", "label"];
const PLATFORMS: &[(&str, &str)] = &[("ubereats.com", "Uber Eats"), ("deliveroo.fr", "Deliveroo"), ("just-eat.fr", "Just Eat"),
    ("thefork.fr", "TheFork"), ("lafourchette.com", "TheFork"), ("bookings.zenchef.com", "Zenchef"), ("zenchef.com/", "Zenchef"),
    ("guestonline.io", "Guestonline"), ("sevenrooms.com", "SevenRooms"), ("covermanager.com", "CoverManager"),
    ("resengo.com", "Resengo"), ("tablebooker.com", "Tablebooker")];

// ---------------------------------------------------------------- polite fetching

struct Robots {
    rules: Vec<(bool, String)>, // (allow, pattern) of the group that applies to us
}
impl Robots {
    fn parse(txt: &str) -> Robots {
        // groups: consecutive User-agent lines, then rules; ours if it names tinynantesbot, else '*'
        let (mut ours, mut star): (Vec<(bool, String)>, Vec<(bool, String)>) = (vec![], vec![]);
        let (mut agents, mut in_rules, mut found_ours): (Vec<String>, bool, bool) = (vec![], false, false);
        for line in txt.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((k, v)) = line.split_once(':') else { continue };
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_string());
            match k.as_str() {
                "user-agent" => {
                    if in_rules {
                        agents.clear();
                        in_rules = false;
                    }
                    agents.push(v.to_ascii_lowercase());
                }
                "allow" | "disallow" => {
                    in_rules = true;
                    if v.is_empty() {
                        continue;
                    }
                    let r = (k == "allow", v);
                    if agents.iter().any(|a| a.contains("tinynantesbot")) {
                        found_ours = true;
                        ours.push(r.clone());
                    }
                    if agents.iter().any(|a| a == "*") {
                        star.push(r);
                    }
                }
                _ => {}
            }
        }
        Robots { rules: if found_ours { ours } else { star } }
    }
    /// longest matching rule wins, allow wins ties
    fn allows(&self, path: &str) -> bool {
        let mut best: Option<(usize, bool)> = None;
        for (allow, pat) in &self.rules {
            if glob(pat, path) {
                let l = pat.len();
                if best.map_or(true, |(bl, ba)| l > bl || (l == bl && *allow && !ba)) {
                    best = Some((l, *allow));
                }
            }
        }
        best.map_or(true, |(_, a)| a)
    }
}

/// robots.txt pattern: prefix match, `*` any run, `$` end
fn glob(pat: &str, s: &str) -> bool {
    let (pat, anchored) = match pat.strip_suffix('$') {
        Some(p) => (p, true),
        None => (pat, false),
    };
    let parts: Vec<&str> = pat.split('*').collect();
    let mut pos = 0;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            if !s.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if let Some(k) = s[pos..].find(part) {
            pos += k + part.len();
        } else {
            return false;
        }
    }
    !anchored || pos == s.len() || (parts.len() > 1 && parts.last() == Some(&"") )
}

// ---------------------------------------------------------------- public internet only
// Place websites come from OSM: never let one point the server at the cluster's own network (SSRF). Names are
// resolved through PublicOnly (every request, redirects included); IP literals are checked before and on redirect.

fn public_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            !(v.is_private() || v.is_loopback() || v.is_link_local() || v.is_unspecified() || v.is_broadcast()
                || v.is_documentation() || o[0] == 0 || (o[0] == 100 && (o[1] & 0xc0) == 64) || o[0] >= 224)
        }
        IpAddr::V6(v) => {
            if let Some(m) = v.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(m));
            }
            let s = v.segments();
            !(v.is_loopback() || v.is_unspecified() || (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80 || v.is_multicast())
        }
    }
}

fn literal_private(u: &Url) -> bool {
    match u.host() {
        Some(url::Host::Ipv4(v)) => !public_ip(v.into()),
        Some(url::Host::Ipv6(v)) => !public_ip(v.into()),
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost") || d.ends_with(".local") || d.ends_with(".internal")
            || d.ends_with(".svc") || d.contains(".svc.") || !d.contains('.'),
        None => true,
    }
}

struct PublicOnly;
impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((name.as_str(), 0)).await?
                .filter(|a| public_ip(a.ip())).collect();
            if addrs.is_empty() {
                return Err("no public address".into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

pub struct Page {
    pub url: Url,
    pub ctype: String,
    pub body: Vec<u8>,
}

pub struct Fetcher {
    http: reqwest::Client,
    robots: Mutex<HashMap<String, Arc<Robots>>>,
    next_slot: Mutex<HashMap<String, Instant>>,
    inflight: Mutex<std::collections::HashSet<String>>,
}

impl Fetcher {
    pub fn new() -> Result<Fetcher> {
        let http = reqwest::Client::builder().user_agent(UA).timeout(Duration::from_secs(20))
            .dns_resolver(Arc::new(PublicOnly))
            .redirect(reqwest::redirect::Policy::custom(|a| {
                if a.previous().len() >= 6 {
                    a.error("too many redirects")
                } else if literal_private(a.url()) {
                    a.stop()
                } else {
                    a.follow()
                }
            }))
            .build()?;
        Ok(Fetcher { http, robots: Default::default(), next_slot: Default::default(), inflight: Default::default() })
    }

    /// mark a place as being fetched; false if it already is
    pub fn begin(&self, id: &str) -> bool {
        self.inflight.lock().unwrap().insert(id.to_string())
    }
    pub fn end(&self, id: &str) {
        self.inflight.lock().unwrap().remove(id);
    }
    pub fn busy(&self, id: &str) -> bool {
        self.inflight.lock().unwrap().contains(id)
    }

    async fn wait_turn(&self, host: &str) {
        let wait = {
            let mut m = self.next_slot.lock().unwrap();
            let now = Instant::now();
            let slot = m.get(host).copied().filter(|t| *t > now).unwrap_or(now);
            m.insert(host.to_string(), slot + Duration::from_secs(1));
            slot - now
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }

    async fn robots_for(&self, u: &Url) -> Arc<Robots> {
        let key = format!("{}://{}", u.scheme(), u.host_str().unwrap_or(""));
        if let Some(r) = self.robots.lock().unwrap().get(&key) {
            return r.clone();
        }
        self.wait_turn(u.host_str().unwrap_or("")).await;
        let txt = match self.http.get(format!("{key}/robots.txt")).send().await {
            Ok(r) if r.status().is_success() => r.text().await.unwrap_or_default(),
            _ => String::new(),
        };
        let r = Arc::new(Robots::parse(&txt));
        self.robots.lock().unwrap().insert(key, r.clone());
        r
    }

    pub async fn get(&self, url: &str) -> Result<Page> {
        let u = Url::parse(url)?;
        if !matches!(u.scheme(), "http" | "https") {
            return Err(anyhow!("scheme"));
        }
        if literal_private(&u) {
            return Err(anyhow!("private address"));
        }
        let path = match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_string(),
        };
        if !self.robots_for(&u).await.allows(&path) {
            return Err(anyhow!("robots.txt"));
        }
        self.wait_turn(u.host_str().unwrap_or("")).await;
        let mut r = self.http.get(u.clone()).header("Accept-Language", "fr-FR,fr;q=0.9").send().await?;
        if !r.status().is_success() {
            return Err(anyhow!("HTTP {}", r.status()));
        }
        let final_url = r.url().clone();
        let ctype = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
        let mut body = Vec::new();
        while let Some(chunk) = r.chunk().await? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_BYTES {
                return Err(anyhow!("too large"));
            }
        }
        Ok(Page { url: final_url, ctype, body })
    }
}

// ---------------------------------------------------------------- extraction

#[derive(Clone, Default)]
struct Item {
    name: String,
    desc: String,
    price: Option<f64>,
}
#[derive(Clone, Default)]
struct Section {
    title: String,
    items: Vec<Item>,
}

fn count(s: &[Section]) -> usize {
    s.iter().map(|x| x.items.len()).sum()
}

fn price_of(s: &str) -> Option<f64> {
    s.chars().filter(|c| !c.is_whitespace()).collect::<String>().replace(',', ".").parse().ok()
}

fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// schema.org Menu in JSON-LD
fn from_jsonld(doc: &Html) -> Vec<Section> {
    let sel = scraper::Selector::parse(r#"script[type="application/ld+json"]"#).unwrap();
    let mut out = vec![];
    for tag in doc.select(&sel) {
        let Ok(v) = serde_json::from_str::<Value>(&tag.text().collect::<String>()) else { continue };
        let mut stack = vec![&v];
        while let Some(d) = stack.pop() {
            match d {
                Value::Array(a) => stack.extend(a.iter()),
                Value::Object(o) => {
                    let t = &o.get("@type").cloned().unwrap_or(Value::Null);
                    let is = |n: &str| t == n || t.as_array().is_some_and(|a| a.iter().any(|x| x == n));
                    if is("Menu") || is("MenuSection") {
                        let secs: Vec<&Value> = match o.get("hasMenuSection") {
                            Some(Value::Array(a)) => a.iter().collect(),
                            Some(x @ Value::Object(_)) => vec![x],
                            _ => vec![d],
                        };
                        for sec in secs {
                            let items: Vec<Item> = sec["hasMenuItem"].as_array().into_iter().flatten().map(|it| {
                                let off = if it["offers"].is_array() { &it["offers"][0] } else { &it["offers"] };
                                let p = match &off["price"] {
                                    Value::Number(n) => n.as_f64(),
                                    Value::String(s) => price_of(s),
                                    _ => None,
                                };
                                Item { name: clean(it["name"].as_str().unwrap_or("")), desc: clean(it["description"].as_str().unwrap_or("")), price: p }
                            }).filter(|i| !i.name.is_empty()).collect();
                            if !items.is_empty() {
                                out.push(Section { title: clean(sec["name"].as_str().unwrap_or("")), items });
                            }
                        }
                    }
                    stack.extend(o.values().filter(|v| v.is_object() || v.is_array()));
                }
                _ => {}
            }
        }
    }
    out
}

/// The page as lines of text, one per block; headings flagged.
fn lines(doc: &Html) -> Vec<(String, bool)> {
    fn walk(el: ElementRef, out: &mut Vec<(String, bool)>, cur: &mut String) {
        for child in el.children() {
            match child.value() {
                Node::Text(t) => {
                    cur.push(' ');
                    cur.push_str(t);
                }
                Node::Element(e) => {
                    let name = e.name();
                    if SKIP.contains(&name) {
                        continue;
                    }
                    let Some(ce) = ElementRef::wrap(child) else { continue };
                    if name.len() == 2 && name.starts_with('h') && name.as_bytes()[1].is_ascii_digit() {
                        flush(out, cur, false);
                        let mut h = String::new();
                        walk(ce, out, &mut h);
                        flush(out, &mut h, true);
                    } else if BLOCK.contains(&name) {
                        flush(out, cur, false);
                        walk(ce, out, cur);
                        flush(out, cur, false);
                    } else {
                        walk(ce, out, cur);
                    }
                }
                _ => {}
            }
        }
    }
    fn flush(out: &mut Vec<(String, bool)>, cur: &mut String, heading: bool) {
        let s = clean(cur);
        cur.clear();
        if !s.is_empty() {
            out.push((s, heading));
        }
    }
    let mut out = vec![];
    let mut cur = String::new();
    walk(doc.root_element(), &mut out, &mut cur);
    flush(&mut out, &mut cur, false);
    out
}

fn is_caps_title(s: &str) -> bool {
    let letters = s.chars().filter(|c| c.is_alphabetic()).count();
    s.len() <= 40 && letters >= 3 && !s.chars().any(|c| c.is_lowercase()) && !PRICE.is_match(s)
}

/// Which pending line is the dish: the first short one, except a capitalised title line followed by a
/// name-like line (short, no comma) or by two lines (name + description).
fn dish_index(pending: &[&str], soft: Option<&str>) -> Option<usize> {
    let k = pending.iter().position(|p| p.chars().count() <= 110)?;
    if soft.is_some_and(|t| pending[k] == t) {
        let rest = &pending[k + 1..];
        let desc_like = |l: &str| l.contains(',') || l.chars().count() > 45 || l.split_whitespace().count() >= 6
            || l.starts_with(|c: char| c.is_lowercase());
        if rest.len() >= 2 || (rest.len() == 1 && !desc_like(rest[0])) {
            return Some(k + 1);
        }
    }
    Some(k)
}

fn good_name(s: &str) -> bool {
    (2..=110).contains(&s.chars().count()) && WORDY.is_match(s) && !JUNK.is_match(s)
}

/// Pair dishes with prices over a sequence of lines.
fn from_lines(lines: &[(String, bool)]) -> Vec<Section> {
    let mut secs: Vec<Section> = vec![];
    let mut cur = Section::default();
    let mut pending: Vec<&str> = vec![];
    let mut seen = std::collections::HashSet::new();
    let mut soft: Option<&str> = None;
    let mut push = |cur: &mut Section, secs: &mut Vec<Section>, soft: &mut Option<&str>, name: &str, desc: String, price: Option<f64>| {
        // a pending capitalised line that is not this dish's name was a section title
        if let Some(t) = soft.take() {
            if t != name {
                if !cur.items.is_empty() {
                    secs.push(std::mem::take(cur));
                }
                cur.title = t.chars().take(80).collect();
            }
        }
        let name = name.trim_matches(|c: char| " -–—.:…|/,·*".contains(c)).to_string();
        if !good_name(&name) || price.is_some_and(|p| !(0.3..=600.0).contains(&p)) {
            return;
        }
        if seen.insert((name.to_lowercase(), price.map(|p| (p * 100.0) as i64))) {
            cur.items.push(Item { name, desc: desc.chars().take(220).collect(), price });
        }
    };
    for (line, heading) in lines {
        let line = line.as_str();
        if *heading {
            if !cur.items.is_empty() {
                secs.push(std::mem::take(&mut cur));
            }
            cur.title = line.chars().take(80).collect();
            pending.clear();
            soft = None;
            continue;
        }
        if is_caps_title(line) {
            // a short line in capitals: a section title, or a dish written in capitals ("LES PINTXOS DU
            // COMMANDANT" / description / 9,50 €). Decided by what comes next.
            if let Some(prev) = soft.replace(line) {
                // two capitalised lines in a row: the first was a section title
                if !cur.items.is_empty() {
                    secs.push(std::mem::take(&mut cur));
                }
                cur.title = prev.chars().take(80).collect();
            }
            pending.clear();
            pending.push(line);
            continue;
        }
        if line.len() <= 40 {
            if let Some(m) = PRICE_ONLY.captures(line) {
                let has_sign = PRICE.is_match(line);
                if has_sign || DECIMALS.is_match(line) {
                    // the dish is the first short pending line; what follows it is its description
                    if let Some(k) = dish_index(&pending, soft) {
                        let desc = pending[k + 1..].join(" ");
                        push(&mut cur, &mut secs, &mut soft, pending[k], desc, price_of(&m[1]));
                    }
                    pending.clear();
                    continue;
                }
            }
        }
        if line.len() <= 220 {
            if let Some(m) = PRICE.captures(line) {
                let price = price_of(m.get(1).or(m.get(2)).map(|g| g.as_str()).unwrap_or(""));
                let rest = PRICE.replace_all(line, " ");
                let rest = clean(&rest);
                // "Name - description 12 €": split a long name at the first dash or colon
                let (name, desc) = match rest.find([':', '–', '—']).filter(|_| rest.chars().count() > 60) {
                    Some(k) => (rest[..k].to_string(), rest[k..].trim_start_matches([':', '–', '—', ' ']).to_string()),
                    None => (rest.clone(), String::new()),
                };
                if good_name(&name) {
                    let desc = if desc.is_empty() { pending.join(" ") } else { desc };
                    let desc = if pending.len() > 2 { String::new() } else { desc };
                    push(&mut cur, &mut secs, &mut soft, &name, desc, price);
                } else if let Some(k) = dish_index(&pending, soft) {
                    // "Bœuf bourguignon | Servi avec … 18 €"
                    let mut d = pending[k + 1..].join(" ");
                    if !rest.is_empty() && !JUNK.is_match(&rest) {
                        d = clean(&format!("{d} {rest}"));
                    }
                    push(&mut cur, &mut secs, &mut soft, pending[k], d, price);
                }
                pending.clear();
                continue;
            }
        }
        if line.chars().count() < 40 && JUNK.is_match(line) {
            continue; // buttons and shop chrome ("+ Add", "Produit à voir")
        }
        if line.chars().count() <= 260 {
            pending.push(line);
            if pending.len() > 3 {
                pending.remove(0);
            }
        } else {
            pending.clear();
        }
    }
    if !cur.items.is_empty() {
        secs.push(cur);
    }
    if count(&secs) >= MIN_ITEMS { secs } else { vec![] }
}

fn pdf_lines(bytes: &[u8]) -> Vec<(String, bool)> {
    let bytes = bytes.to_vec();
    let text = std::panic::catch_unwind(move || pdf_extract::extract_text_from_mem(&bytes).ok()).ok().flatten().unwrap_or_default();
    text.lines().map(clean).filter(|l| !l.is_empty()).map(|l| (l, false)).collect()
}

fn sections_json(s: &[Section]) -> Value {
    Value::Array(s.iter().take(20).map(|sec| json!({
        "title": sec.title,
        "items": sec.items.iter().take(80).map(|i| {
            let mut o = json!({"name": i.name, "price": i.price});
            if !i.desc.is_empty() { o["desc"] = json!(i.desc); }
            o
        }).collect::<Vec<_>>()
    })).collect())
}

/// What a page links to: same-site menu pages, PDF menus, menu images, delivery/booking platforms.
struct Links {
    pages: Vec<String>,
    pdfs: Vec<String>,
    images: Vec<String>,
    /// every other content picture (no logo, icon, social badge): the menu when a menu page has no text one
    pictures: Vec<String>,
    platforms: Vec<(String, String)>,
    description: Option<String>,
    image: Option<String>,
}

fn links(doc: &Html, base: &Url) -> Links {
    let a_sel = scraper::Selector::parse("a[href]").unwrap();
    let img_sel = scraper::Selector::parse("img").unwrap();
    let meta_sel = scraper::Selector::parse("meta").unwrap();
    let mut l = Links { pages: vec![], pdfs: vec![], images: vec![], pictures: vec![], platforms: vec![], description: None, image: None };
    let not_pic = Regex::new(r"(?i)logo|icon|instagram|facebook|tripadvisor|google|twitter|avatar|\.svg|\.gif|pixel|badge|sprite").unwrap();
    let host = base.host_str().unwrap_or("").trim_start_matches("www.").to_string();
    for a in doc.select(&a_sel) {
        let Some(u) = a.value().attr("href").and_then(|h| base.join(h).ok()) else { continue };
        if !matches!(u.scheme(), "http" | "https") {
            continue;
        }
        let mut u2 = u.clone();
        u2.set_fragment(None);
        let s = u2.to_string();
        let label = format!("{} {}", a.text().collect::<String>(), u.path()).to_lowercase();
        if let Some((_, name)) = PLATFORMS.iter().find(|(d, _)| s.contains(d)) {
            if !l.platforms.iter().any(|(n, _)| n == name) {
                l.platforms.push((name.to_string(), s));
            }
            continue;
        }
        let uhost = u.host_str().unwrap_or("").trim_start_matches("www.");
        if u.path().to_lowercase().ends_with(".pdf") {
            if (MENU_WORDS.is_match(&label) || uhost == host) && !l.pdfs.contains(&s) {
                l.pdfs.push(s);
            }
        } else if uhost == host && MENU_WORDS.is_match(&label) && u2.as_str().trim_end_matches('/') != base.as_str().trim_end_matches('/')
            && !l.pages.contains(&s) && !s.contains("/wp-content/") && !s.contains("/feed") {
            l.pages.push(s);
        }
    }
    for img in doc.select(&img_sel) {
        let e = img.value();
        let src = e.attr("src").or(e.attr("data-src")).unwrap_or("");
        let alt = format!("{} {} {}", e.attr("alt").unwrap_or(""), e.attr("title").unwrap_or(""), src).to_lowercase();
        let Some(s) = base.join(src).ok().map(|u| u.to_string()).filter(|s| !src.is_empty() && !s.starts_with("data:")) else { continue };
        if Regex::new(r"(la[-_ ]?carte|\bcarte\b|menu)").unwrap().is_match(&alt) && !alt.contains("logo") {
            if !l.images.contains(&s) {
                l.images.push(s);
            }
        } else if !not_pic.is_match(&alt) && !l.pictures.contains(&s) {
            l.pictures.push(s);
        }
    }
    for m in doc.select(&meta_sel) {
        let e = m.value();
        let key = e.attr("property").or(e.attr("name")).unwrap_or("");
        let Some(c) = e.attr("content") else { continue };
        match key {
            "og:description" | "description" if l.description.is_none() => l.description = Some(clean(c).chars().take(400).collect()),
            "og:image" | "twitter:image" if l.image.is_none() => l.image = base.join(c).ok().map(|u| u.to_string()),
            _ => {}
        }
    }
    l
}

fn url_path(u: &Url) -> &str {
    u.path()
}

/// Read a place's website: returns the JSON the viewer's panel shows.
pub async fn fetch_place(f: &Fetcher, id: &str, name: &str, kind: &str, website: &str) -> Value {
    let site = if website.starts_with("http") { website.to_string() } else { format!("https://{website}") };
    let mut out = json!({"id": id, "name": name, "kind": kind, "website": site, "fetched": chrono::Utc::now().timestamp(),
        "sections": [], "pdfs": [], "images": [], "links": [], "menu_url": null, "source": null, "pages": []});
    let home = match f.get(&site).await {
        Ok(p) => p,
        Err(e) => {
            out["error"] = json!(format!("site injoignable ({e})"));
            return out;
        }
    };
    let mut best: (Vec<Section>, String, &str) = (vec![], String::new(), "");
    let consider = |secs: Vec<Section>, url: &str, src: &'static str, best: &mut (Vec<Section>, String, &str)| {
        if count(&secs) > count(&best.0) {
            *best = (secs, url.to_string(), src);
        }
    };
    let (mut pdfs, mut images, mut platforms, mut pages) = (vec![], vec![], vec![], vec![]);
    let mut queue = vec![home];
    let mut visited = 0;
    while let Some(p) = queue.pop() {
        visited += 1;
        pages.push(p.url.to_string());
        if !p.ctype.contains("html") {
            continue;
        }
        let html = String::from_utf8_lossy(&p.body).to_string();
        // parsing is CPU work: keep it off the async threads, and the !Send Html inside this block
        let url = p.url.clone();
        let (jl, tx, l) = tokio::task::spawn_blocking(move || {
            let doc = Html::parse_document(&html);
            (from_jsonld(&doc), from_lines(&lines(&doc)), links(&doc, &url))
        }).await.unwrap_or_else(|_| (vec![], vec![], Links { pages: vec![], pdfs: vec![], images: vec![], pictures: vec![], platforms: vec![], description: None, image: None }));
        // a menu page without a text menu: its pictures are the menu (photographed or designed cards)
        let menu_page = visited > 1 && Regex::new(r"(?i)carte|menu|food|plats|drink|boissons|vins|brunch").unwrap().is_match(url_path(&p.url));
        if menu_page && count(&tx) < MIN_ITEMS && count(&jl) < MIN_ITEMS && l.pictures.len() <= 12 {
            for x in &l.pictures { if !images.contains(x) { images.push(x.clone()) } }
        }
        consider(jl, p.url.as_str(), "jsonld", &mut best);
        consider(tx, p.url.as_str(), "text", &mut best);
        if visited == 1 {
            out["description"] = json!(l.description);
            out["image"] = json!(l.image);
            // follow up to 4 menu pages of the same site, most menu-like first
            let mut cand = l.pages.clone();
            cand.sort_by_key(|u| if Regex::new(r"(?i)carte|menu").unwrap().is_match(u) { 0 } else { 1 });
            for u in cand.into_iter().take(4).rev() {
                if let Ok(p2) = f.get(&u).await {
                    queue.push(p2);
                }
            }
        }
        for x in l.pdfs { if !pdfs.contains(&x) { pdfs.push(x) } }
        for x in l.images { if !images.contains(&x) { images.push(x) } }
        for x in l.platforms { if !platforms.iter().any(|(n, _): &(String, String)| *n == x.0) { platforms.push(x) } }
    }
    if count(&best.0) < 8 {
        for u in pdfs.iter().take(3) {
            if let Ok(p) = f.get(u).await {
                if p.body.starts_with(b"%PDF") {
                    let secs = tokio::task::spawn_blocking(move || from_lines(&pdf_lines(&p.body))).await.unwrap_or_default();
                    consider(secs, u, "pdf", &mut best);
                }
            }
        }
    }
    out["sections"] = sections_json(&best.0);
    if !best.1.is_empty() {
        out["menu_url"] = json!(best.1);
        out["source"] = json!(best.2);
    }
    out["pdfs"] = json!(pdfs.into_iter().take(5).collect::<Vec<_>>());
    out["images"] = json!(images.into_iter().take(6).collect::<Vec<_>>());
    out["links"] = json!(platforms.into_iter().map(|(n, u)| json!({"name": n, "url": u})).collect::<Vec<_>>());
    out["pages"] = json!(pages);
    out
}

/// debugging: the text lines of a page as the extractor sees them
pub async fn debug_lines(f: &Fetcher, url: &str) -> Vec<(String, bool)> {
    match f.get(url).await {
        Ok(p) => lines(&Html::parse_document(&String::from_utf8_lossy(&p.body))),
        Err(e) => vec![(format!("{e}"), true)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ls(v: &[&str]) -> Vec<(String, bool)> {
        v.iter().map(|s| (s.to_string(), false)).collect()
    }

    #[test]
    fn price_on_its_own_line() {
        let s = from_lines(&ls(&["ENTRÉE", "Entrée de la semaine", "Aubergine panée façon tataki", "7 €",
            "Œufs mayonnaise bloody César", "Piment fumé et sel de céleri.", "5,50 €", "PLAT", "Plat du jour", "15 €",
            "Burger maison", "18,50"]));
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].title, "ENTRÉE");
        assert_eq!(s[0].items[0].name, "Entrée de la semaine");
        assert_eq!(s[0].items[0].desc, "Aubergine panée façon tataki");
        assert_eq!(s[0].items[1].price, Some(5.5));
        assert_eq!(s[1].items[1].price, Some(18.5));
    }

    #[test]
    fn dishes_in_capitals() {
        let s = from_lines(&ls(&["ENTRÉES", "L'ŒUF MOLLET", "Œuf mollet frit, lardons", "8,50 €",
            "LES PINTXOS DU COMMANDANT", "Pain grillé, saumon fumé", "9,50 €", "DESSERTS", "Tarte du jour 6 €",
            "Café gourmand 7 €"]));
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].title, "ENTRÉES");
        assert_eq!(s[0].items[1].name, "LES PINTXOS DU COMMANDANT");
        assert_eq!(s[1].title, "DESSERTS");
        let s = from_lines(&ls(&["Pizza Salmone", "+ Add 17,90 €", "Pizza 4 fromages", "+ Add 16,90 €", "Lasagnes", "+ Add 18,90 €",
            "Burrata", "+ Add 18,50 €"]));
        assert_eq!(s[0].items[0].name, "Pizza Salmone");
    }

    #[test]
    fn inline_prices() {
        let s = from_lines(&ls(&["Galette complète 9,50 €", "Crêpe beurre sucre 4 €", "Cidre brut 4,5 €", "Café 2 €", "Livraison 3 €"]));
        assert_eq!(count(&s), 4);
        assert_eq!(s[0].items[0].name, "Galette complète");
    }

    #[test]
    fn private_addresses() {
        for u in ["http://127.0.0.1/", "http://10.0.0.5/menu", "http://192.168.1.1/", "http://169.254.169.254/latest",
                  "http://[::1]/", "http://[fd00::1]/", "http://localhost:8080/", "http://kubernetes.default.svc/", "http://minix/"] {
            assert!(literal_private(&Url::parse(u).unwrap()), "{u}");
        }
        assert!(!literal_private(&Url::parse("https://www.mrmachin.com/").unwrap()));
        assert!(!literal_private(&Url::parse("http://93.184.216.34/").unwrap()));
    }

    #[test]
    fn robots() {
        let r = Robots::parse("User-agent: *\nDisallow: /admin\nAllow: /admin/menu\n\nUser-agent: GPTBot\nDisallow: /\n");
        assert!(r.allows("/carte"));
        assert!(!r.allows("/admin/x"));
        assert!(r.allows("/admin/menu.pdf"));
        let r = Robots::parse("User-agent: TinyNantesBot\nDisallow: /\nUser-agent: *\nAllow: /\n");
        assert!(!r.allows("/"));
        let r = Robots::parse("User-agent: *\nDisallow: /*.pdf$\n");
        assert!(!r.allows("/a/menu.pdf"));
        assert!(r.allows("/a/menu.pdf?x"));
    }
}
