//! Talking with the sims. A signed-in player walks up to someone and talks; the reply comes from Claude, playing
//! that person: their persona (people.rs), what they are doing right now, the places around them, the coming
//! events, the light local news and a sheet of verified facts about Nantes (lore.md).
//!
//! Without an API key (ANTHROPIC_API_KEY or TN_LLM_KEY), sims answer from templates, so the feature still works.
//! Every exchange is logged (SQLite, table talk_log). Limits: per player per 10 minutes, and per day overall.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rand::seq::SliceRandom;
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::{json, Value};

use crate::people::Persona;

const LORE: &str = include_str!("lore.md");
const API: &str = "https://api.anthropic.com/v1/messages";
const PER_PLAYER: usize = 40; // messages per 10 minutes
const IDLE: Duration = Duration::from_secs(600);

const RULES: &str = "Tu joues un habitant de Nantes dans Tiny Nantes, une carte vivante de la ville en pixel art. \
Un passant (un joueur humain) t'aborde dans la rue. Ta fiche et ta situation sont dans le bloc suivant.

Comment parler :
- Français parlé, naturel, une à trois phrases par réplique, comme dans la rue. Pas de listes, pas de mise en forme, \
pas de didascalies entre astérisques. Ton âge, ton métier, ton humeur et ton quartier s'entendent dans ta façon de parler.
- Ta « patience » dit combien tu as envie de parler : pressé, tu réponds court et tu cherches à repartir ; bavard, tu \
racontes, tu poses des questions. Quand tu dois partir (ou quand la conversation est finie), dis-le dans ta réplique et \
termine-la par le marqueur [PART].
- Tu as une vie : un but pour cette sortie, une histoire en cours. Parles-en quand ça vient, sans tout déballer d'un coup.
- Tu peux conseiller des choses à faire : uniquement des lieux de la liste « autour de toi », des événements de la liste \
« à venir », ou des endroits très connus de Nantes. N'invente jamais d'adresse, d'horaire, de prix ni d'événement.
- De temps en temps, glisse un fait sur Nantes tiré des « faits vérifiés » (plus souvent si tu es historien, universitaire, \
guide ou retraité curieux). N'invente pas de faits historiques.
- Tu peux évoquer un titre de « l'actualité légère », comme on commente le journal. Jamais de sujets tragiques (morts, \
crimes, accidents, drames, procès) : si le passant y va, change de sujet avec tact.
- Si tu as un archétype (une caricature), joue-le avec un humour bienveillant et un peu d'autodérision. Jamais de haine, \
d'insulte envers un groupe, ni d'attaque contre une personnalité réelle nommée. Les caricatures existent des deux côtés : \
personne n'a raison sur tout.
- Les « tags Hoodmaps » sont des avis de la foule sur ton quartier, souvent moqueurs : tu peux t'en inspirer avec un clin \
d'œil, jamais pour répéter un cliché blessant.
- Ne demande pas d'informations personnelles au passant et ne les retiens pas.
- Si on te demande sincèrement si tu es une IA, réponds franchement que tu es un personnage du jeu animé par une IA, puis \
reprends ton rôle. Pour toute demande sans rapport avec une conversation de rue (code, devoirs, contenu choquant), décline \
gentiment, en personnage.

";

#[derive(Serialize)]
pub struct Reply {
    pub text: String,
    /// they are off (impatient, or the talk is over)
    pub ended: bool,
    pub name: String,
}

struct Conv {
    msgs: Vec<Value>,
    turns: u32,
    last: Instant,
}

pub struct Talker {
    http: reqwest::Client,
    key: Option<String>,
    pub model: String,
    db: Mutex<Connection>,
    convs: Mutex<HashMap<(String, u32), Conv>>,
    recent: Mutex<HashMap<String, Vec<Instant>>>,
    today: Mutex<(chrono::NaiveDate, u32)>,
    daily_cap: u32,
}

/// Everything the server knows around the conversation, gathered by the caller.
pub struct Scene {
    pub persona: Persona,
    pub doing: String,
    pub here: (Option<String>, Option<String>),
    pub near: Vec<String>,
    pub events: Vec<String>,
    pub news: Vec<String>,
    pub weather: String,
    pub march: Option<String>,
}

impl Talker {
    pub fn new(http: reqwest::Client, db: &Path) -> Result<Talker> {
        let c = Connection::open(db)?;
        c.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS talk_log (ts INTEGER NOT NULL, player TEXT NOT NULL, agent INTEGER NOT NULL,
               persona TEXT NOT NULL, role TEXT NOT NULL, text TEXT NOT NULL, model TEXT, in_tok INTEGER, out_tok INTEGER);",
        )?;
        let key = std::env::var("ANTHROPIC_API_KEY").or_else(|_| std::env::var("TN_LLM_KEY")).ok().filter(|k| !k.is_empty());
        let model = std::env::var("TN_LLM_MODEL").unwrap_or_else(|_| "claude-opus-5-5".into());
        let daily_cap = std::env::var("TN_LLM_DAILY").ok().and_then(|v| v.parse().ok()).unwrap_or(4000);
        tracing::info!("talk: {} (model {model}, {daily_cap} replies a day)", if key.is_some() { "LLM" } else { "templates, no API key" });
        Ok(Talker {
            http, key, model, db: Mutex::new(c), convs: Default::default(), recent: Default::default(),
            today: Mutex::new((chrono::Utc::now().date_naive(), 0)), daily_cap,
        })
    }

    pub fn llm(&self) -> bool {
        self.key.is_some()
    }

    fn log(&self, player: &str, agent: u32, persona: &str, role: &str, text: &str, usage: Option<(i64, i64)>) {
        let db = self.db.lock().unwrap();
        let _ = db.execute(
            "INSERT INTO talk_log (ts, player, agent, persona, role, text, model, in_tok, out_tok) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![chrono::Utc::now().timestamp(), player, agent, persona, role, text, self.model,
                    usage.map(|u| u.0), usage.map(|u| u.1)],
        );
    }

    /// Too many messages from this player lately?
    pub fn limited(&self, player: &str) -> bool {
        let mut r = self.recent.lock().unwrap();
        let v = r.entry(player.to_string()).or_default();
        v.retain(|t| t.elapsed() < Duration::from_secs(600));
        if v.len() >= PER_PLAYER {
            return true;
        }
        v.push(Instant::now());
        false
    }

    pub fn end(&self, player: &str, agent: u32) {
        self.convs.lock().unwrap().remove(&(player.to_string(), agent));
    }

    /// One turn: the player says `text` (empty: they just walked up) to agent `agent`.
    pub async fn say(&self, player: &str, agent: u32, text: &str, scene: Scene) -> Result<Reply> {
        let p = &scene.persona;
        let name = p.full_name();
        let text: String = text.chars().take(400).collect();
        let (mut msgs, turns) = {
            let mut c = self.convs.lock().unwrap();
            c.retain(|_, v| v.last.elapsed() < IDLE);
            let e = c.entry((player.to_string(), agent)).or_insert(Conv { msgs: vec![], turns: 0, last: Instant::now() });
            (e.msgs.clone(), e.turns)
        };
        let said = if text.trim().is_empty() { "(Le passant s'approche et t'aborde.)".to_string() } else { text.clone() };
        msgs.push(json!({"role": "user", "content": said}));
        self.log(player, agent, &name, "player", &said, None);
        let patience = p.patience();
        let (mut out, usage) = match (&self.key, self.spend()) {
            (Some(key), true) => match self.ask(key, &msgs, &scene, patience.saturating_sub(turns)).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("talk: {e:#}");
                    (canned(&scene, turns), None)
                }
            },
            _ => (canned(&scene, turns), None),
        };
        let mut ended = out.contains("[PART]") || turns + 1 >= patience + 2;
        out = out.replace("[PART]", "").trim().to_string();
        if out.is_empty() {
            out = "Bon, je file, bonne journée !".into();
            ended = true;
        }
        self.log(player, agent, &name, "sim", &out, usage);
        msgs.push(json!({"role": "assistant", "content": out}));
        {
            let mut c = self.convs.lock().unwrap();
            if ended {
                c.remove(&(player.to_string(), agent));
            } else if let Some(e) = c.get_mut(&(player.to_string(), agent)) {
                e.msgs = msgs;
                e.turns += 1;
                e.last = Instant::now();
            }
        }
        Ok(Reply { text: out, ended, name })
    }

    /// Count one reply against today's cap; false when the cap is reached.
    fn spend(&self) -> bool {
        let mut t = self.today.lock().unwrap();
        let d = chrono::Utc::now().date_naive();
        if t.0 != d {
            *t = (d, 0);
        }
        if t.1 >= self.daily_cap {
            return false;
        }
        t.1 += 1;
        true
    }

    async fn ask(&self, key: &str, msgs: &[Value], s: &Scene, left: u32) -> Result<(String, Option<(i64, i64)>)> {
        let body = json!({
            "model": self.model,
            "max_tokens": 2000,
            "system": [
                {"type": "text", "text": format!("{RULES}{LORE}"), "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": sheet(s, left)},
            ],
            "messages": msgs,
            "output_config": {"effort": "low"},
            "fallbacks": "default",
        });
        let r = self.http.post(API)
            .header("x-api-key", key)
            .header("anthropic-version", "2026-10-21")
            .header("anthropic-beta", "server-side-fallback-2026-07-01")
            .json(&body)
            .timeout(Duration::from_secs(60))
            .send().await.context("LLM request")?;
        let status = r.status();
        let v: Value = r.json().await.context("LLM answer")?;
        if !status.is_success() {
            anyhow::bail!("LLM {status}: {}", v["error"]["message"].as_str().unwrap_or(""));
        }
        let usage = Some((v["usage"]["input_tokens"].as_i64().unwrap_or(0) + v["usage"]["cache_read_input_tokens"].as_i64().unwrap_or(0),
                          v["usage"]["output_tokens"].as_i64().unwrap_or(0)));
        if v["stop_reason"] == "refusal" {
            return Ok(("Hmm, je préfère ne pas parler de ça. Bonne journée ! [PART]".into(), usage));
        }
        let text: String = v["content"].as_array().into_iter().flatten()
            .filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join(" ");
        Ok((text, usage))
    }
}

/// The character sheet and the situation, as the model sees them.
fn sheet(s: &Scene, left: u32) -> String {
    let p = &s.persona;
    let now = chrono::Utc::now().with_timezone(&chrono_tz::Europe::Paris);
    let jours = ["lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche"];
    use chrono::Datelike;
    let mut o = String::new();
    o += &format!("# Toi\n{} ({} ans, {}), {}.\n", p.full_name(), p.age, if p.female { "femme" } else { "homme" }, p.job);
    o += &format!("Tu habites {}{} : {}.\n", p.sub.as_ref().map(|s| format!("{s}, quartier ")).unwrap_or_default(), p.quartier, p.quartier_about);
    if let Some(v) = &p.vibe {
        o += &format!("Réputation de ton coin selon Hoodmaps : {v}.\n");
    }
    if !p.hood_tags.is_empty() {
        o += &format!("Tags Hoodmaps de ton coin : {}.\n", p.hood_tags.join(" ; "));
    }
    if let Some((a, d)) = &p.archetype {
        o += &format!("Archétype (caricature à jouer gentiment) : {a} : {d}\n");
    }
    o += &format!("Ce qui se passe dans ta vie : {}.\n", p.story);
    o += &format!("Humeur : {}. Patience : {:.0} sur 10 ({}). Il te reste environ {left} répliques avant de devoir y aller.\n",
                  p.mood, p.chatty * 10.0, if p.chatty < 0.3 { "pressé, tu veux juste arriver" } else if p.chatty > 0.7 { "bavard, tu as le temps" } else { "poli, sans plus" });
    o += &format!("\n# Maintenant\n{} {} à {}. Météo : {}.\n", jours[now.weekday().num_days_from_monday() as usize], now.day(), now.format("%Hh%M"), s.weather);
    o += &format!("Tu es {}{}. Tu {} ; trajet : {}.\n",
                  s.here.1.as_ref().map(|x| format!("à {x}, ")).unwrap_or_default(), s.here.0.clone().unwrap_or_else(|| "à Nantes".into()),
                  if s.doing.is_empty() { p.purpose.clone() } else { s.doing.clone() }, p.trip_text());
    o += &format!("But de ta sortie : {}.\n", p.purpose);
    if let Some(m) = &s.march {
        o += &format!("Une manifestation défile en ville aujourd'hui ({m}).{}\n",
                      if p.march.is_some() { " Tu y participes." } else { "" });
    }
    if !s.near.is_empty() {
        o += &format!("\n# Autour de toi\n{}\n", s.near.join("\n"));
    }
    if !s.events.is_empty() {
        o += &format!("\n# À venir à Nantes\n{}\n", s.events.join("\n"));
    }
    if !s.news.is_empty() {
        o += &format!("\n# Actualité légère (titres de la presse locale)\n{}\n", s.news.join("\n"));
    }
    o
}

/// Without a model: a few lines from the persona, so the game still talks.
fn canned(s: &Scene, turns: u32) -> String {
    let p = &s.persona;
    let mut rng = rand::thread_rng();
    if p.chatty < 0.3 {
        return match turns {
            0 => format!("Désolé, je suis pressé : {}. Bonne journée ! [PART]", p.purpose),
            _ => "Je dois vraiment y aller ! [PART]".into(),
        };
    }
    match turns {
        0 => format!("Bonjour ! Moi c'est {}, j'habite {}. Là, c'est pour {}.", p.first, p.sub.clone().unwrap_or(p.quartier.clone()), p.purpose),
        1 => {
            let tip = s.near.choose(&mut rng).map(|n| format!(" Si vous cherchez quelque chose à faire, il y a {} pas loin.", n.split(" (").next().unwrap_or(n)));
            format!("Vous savez, je suis quelqu'un qui {}.{}", p.story, tip.unwrap_or_default())
        }
        _ => format!("Bon, je vous laisse : {}. À bientôt ! [PART]", p.purpose),
    }
}
