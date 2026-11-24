//! Who the inhabitants are: a persona for every pedestrian and cyclist (name, age, job, home quartier, why they are
//! out, where they go, how, a storyline, how chatty they are), and how they cross town (on foot, or by tram, bus or
//! Navibus on today's real timetable).
//!
//! Everything about a persona is drawn from its seed, so it is stable for the agent's life. The neighbourhoods
//! come from OSM (quartiers and sub-quartiers of Nantes, Saint-Herblain, Bouguenais) with a hand-written profile
//! each, and the Hoodmaps crowd's vibe and (vetted) tags per sub-quartier (web/data/world/quartiers.json).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};

use crate::gtfs::Timetable;

// ---------------------------------------------------------------- neighbourhoods

#[derive(Deserialize, Clone)]
pub struct Sub {
    pub name: String,
    pub poly: Vec<Vec<[f32; 2]>>,
    pub area: f64,
    #[serde(default)]
    pub vibe: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Deserialize, Clone)]
pub struct Quartier {
    pub name: String,
    pub poly: Vec<Vec<[f32; 2]>>,
    pub area: f64,
    #[serde(default)]
    pub about: String,
    #[serde(default)]
    pub jobs: HashMap<String, f32>,
    #[serde(default)]
    pub subs: Vec<Sub>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Deserialize, Default)]
pub struct Quartiers {
    #[serde(default)]
    pub quartiers: Vec<Quartier>,
}

/// People per area, relative to Nantes (the neighbouring communes are much less dense).
fn density(name: &str) -> f64 {
    match name {
        "Saint-Herblain" => 0.35,
        "Bouguenais" => 0.12,
        _ => 1.0,
    }
}

/// French contractions after a place name was dropped into a sentence ("à Le Lieu Unique" -> "au Lieu Unique").
pub fn contract(s: &str) -> String {
    s.replace("à Le ", "au ").replace("à Les ", "aux ").replace("au Le ", "au ").replace("au La ", "à La ")
        .replace("au L'", "à L'").replace("au Les ", "aux ").replace("de Le ", "du ").replace("de Les ", "des ")
        .replace("au l'", "à l'").replace("au la ", "à la ")
}

fn inside(poly: &[Vec<[f32; 2]>], x: f32, y: f32) -> bool {
    poly.iter().any(|ring| {
        let mut c = false;
        let n = ring.len();
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + n - 1) % n]);
            if (a[1] > y) != (b[1] > y) && x < (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1]) + a[0] {
                c = !c;
            }
        }
        c
    })
}

fn bbox(poly: &[Vec<[f32; 2]>]) -> [f32; 4] {
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for r in poly {
        for p in r {
            b = [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])];
        }
    }
    b
}

impl Quartiers {
    pub fn load(web: &Path) -> Quartiers {
        std::fs::read(web.join("data/world/quartiers.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    /// (quartier, sub-quartier) at a point
    pub fn at(&self, x: f32, y: f32) -> Option<(usize, Option<usize>)> {
        let q = self.quartiers.iter().position(|q| inside(&q.poly, x, y))?;
        let s = self.quartiers[q].subs.iter().position(|s| inside(&s.poly, x, y));
        Some((q, s))
    }
    /// a random point inside a quartier (or one of its sub-quartiers)
    pub fn random_point(&self, rng: &mut impl Rng, q: usize, sub: Option<usize>) -> Option<[f32; 2]> {
        let poly = match sub {
            Some(s) => &self.quartiers[q].subs[s].poly,
            None => &self.quartiers[q].poly,
        };
        let b = bbox(poly);
        for _ in 0..40 {
            let (x, y) = (rng.gen_range(b[0]..b[2].max(b[0] + 1.0)), rng.gen_range(b[1]..b[3].max(b[1] + 1.0)));
            if inside(poly, x, y) {
                return Some([x, y]);
            }
        }
        None
    }
    /// a home quartier, by area times density (a rough population proxy), and a sub-quartier in it
    pub fn pick_home(&self, rng: &mut impl Rng) -> Option<(usize, Option<usize>)> {
        let total: f64 = self.quartiers.iter().map(|q| q.area * density(&q.name)).sum();
        if total <= 0.0 {
            return None;
        }
        let mut r = rng.gen::<f64>() * total;
        let q = self.quartiers.iter().position(|q| { r -= q.area * density(&q.name); r <= 0.0 }).unwrap_or(0);
        let subs = &self.quartiers[q].subs;
        if subs.is_empty() {
            return Some((q, None));
        }
        let st: f64 = subs.iter().map(|s| s.area).sum();
        let mut r = rng.gen::<f64>() * st;
        Some((q, Some(subs.iter().position(|s| { r -= s.area; r <= 0.0 }).unwrap_or(0))))
    }
}

// ---------------------------------------------------------------- personas

const F_YOUNG: &[&str] = &["Léa", "Manon", "Chloé", "Inès", "Jade", "Louise", "Alice", "Lina", "Zoé", "Juliette", "Lucie",
    "Margaux", "Clara", "Sarah", "Nina", "Elsa", "Nolwenn", "Morgane", "Anaïs", "Maëlys", "Yasmine", "Aïssata", "Linh", "Camille"];
const M_YOUNG: &[&str] = &["Gabriel", "Louis", "Raphaël", "Jules", "Adam", "Lucas", "Léo", "Hugo", "Arthur", "Nathan", "Paul",
    "Tom", "Noah", "Sacha", "Malo", "Yanis", "Maël", "Gaspard", "Erwan", "Ronan", "Brieuc", "Mathis", "Moussa", "Karim", "Minh"];
const F_MID: &[&str] = &["Nathalie", "Sandrine", "Isabelle", "Céline", "Sophie", "Gwenaëlle", "Stéphanie", "Aurélie", "Soizic",
    "Karine", "Laurence", "Fatima", "Valérie", "Émilie", "Hélène", "Nadia", "Claire"];
const M_MID: &[&str] = &["Stéphane", "Nicolas", "Olivier", "Christophe", "Yann", "Loïc", "Sébastien", "Frédéric", "Erwan",
    "Thierry", "David", "Karim", "Pascal", "Julien", "Jérôme", "Samir", "Arnaud"];
const F_OLD: &[&str] = &["Monique", "Françoise", "Annick", "Yvonne", "Jacqueline", "Martine", "Marie-Thérèse", "Odile",
    "Bernadette", "Josiane", "Maryvonne", "Colette", "Huguette"];
const M_OLD: &[&str] = &["Jean", "Michel", "Bernard", "Gérard", "Alain", "Jean-Yves", "Daniel", "René", "Joseph", "Yves",
    "Marcel", "Jean-Claude", "Hervé"];
const SURNAMES: &[&str] = &["Le Goff", "Guérin", "Martin", "Leroux", "Le Bihan", "Moreau", "Garnier", "Rousseau", "Fournier",
    "Lambert", "Durand", "Petit", "Leroy", "Brunet", "Hervé", "Guillou", "Tessier", "Bouvier", "Chevalier", "Ménard", "Pichon",
    "Morin", "Gautier", "Caillaud", "Bertin", "Le Roux", "Perrin", "Baudry", "Barbier", "Diallo", "Nguyen", "Haddad",
    "Benali", "Traoré", "Da Silva", "Kaya", "Mendy", "Rakoto", "Gicquel", "Le Floch", "Jaouen", "Riou", "Bréger", "Joubert",
    "Cormerais", "Rialland", "Gouzer", "Musset", "Lebreton", "Ouvrard"];

/// Caricatures, gently mocked, as many on each side (and a few that are on no side at all).
pub const ARCHETYPES: &[(&str, &str)] = &[
    ("militant de gauche", "militant de gauche très engagé : tracts dans la sacoche, parle de « convergence des luttes » et \
      de « rapport de force », trouve que tout est politique, même ta baguette. Sincère, un peu lassant, drôle malgré lui."),
    ("bobo de l'île de Nantes", "bobo de l'île de Nantes : vélo cargo, kombucha maison, appelle tout « tiers-lieu », \
      a un avis sur chaque nouveau food court et dit « c'est hyper inspirant »."),
    ("étudiant en AG", "étudiant en socio qui sort d'une AG et y retourne à 18 h, vote toujours la grève, \
      parle de « déconstruire » et cite Bourdieu au troisième café."),
    ("nostalgique de la ZAD", "ancien de Notre-Dame-des-Landes, parle permaculture, cabanes et « autonomie », \
      se méfie des smartphones (mais en a un)."),
    ("retraité des Hauts-Pavés", "retraité en Barbour des Hauts-Pavés qui peste contre les pistes cyclables, la ZFE \
      et « les trottinettes », trouve que « c'était mieux avant » et le dit poliment."),
    ("jeune start-uper", "jeune entrepreneur « start-up nation » en baskets blanches, pitch sa « solution disruptive » à \
      tout le monde, parle en anglicismes (« on scale », « c'est un game changer »)."),
    ("notable de Guist'hau", "notable de Guist'hau qui ne jure que par La Baule et son voilier, parle de « nos amis du \
      Cercle » et trouve le centre-ville « un peu agité ces temps-ci »."),
    ("commerçant ronchon", "commerçant du centre qui accuse tour à tour les manifs, les travaux et les plateformes de \
      livraison de couler son chiffre, mais connaît tout le monde et rend service."),
    ("supporter des Canaris", "supporter du FC Nantes, pessimiste professionnel, a un avis sur chaque transfert et \
      raconte les années du « jeu à la nantaise » comme une épopée."),
    ("Parisien fraîchement installé", "Parisien installé depuis six mois, compare tout à Paris, découvre la pluie \
      nantaise, dit « c'est tellement plus humain ici »."),
];

#[derive(Clone, Serialize)]
pub enum Trip {
    Walk,
    Bike,
    /// on a Naolib Vélo taken at this station
    Rental { from: String },
    /// on a tram, bus or Navibus: line, kind ("tram", "bus", "navibus"), from stop, to stop
    Transit { line: String, kind: String, from: String, to: String },
}

#[derive(Clone, Serialize)]
pub struct Persona {
    pub first: String,
    pub last: String,
    pub age: u8,
    pub female: bool,
    pub job: String,
    /// a caricature, played for gentle laughs
    pub archetype: Option<(String, String)>,
    pub quartier: String,
    pub sub: Option<String>,
    pub quartier_about: String,
    pub vibe: Option<String>,
    pub hood_tags: Vec<String>,
    /// 0 (in a hurry, curt) .. 1 (all the time in the world, loves to chat)
    pub chatty: f32,
    pub mood: String,
    pub purpose: String,
    pub destination: Option<String>,
    pub story: String,
    pub trip: Trip,
    /// in a demonstration: "manifestant", "batucada", "pancarte"
    pub march: Option<String>,
}

impl Persona {
    /// how many replies before they have to go (the pressed ones leave quickly)
    pub fn patience(&self) -> u32 {
        2 + (self.chatty * 10.0) as u32
    }
    pub fn full_name(&self) -> String {
        format!("{} {}", self.first, self.last)
    }
    pub fn trip_text(&self) -> String {
        match &self.trip {
            Trip::Walk => "à pied".into(),
            Trip::Bike => "à vélo".into(),
            Trip::Rental { from } => format!("en Naolib Vélo pris à la station {from}"),
            Trip::Transit { line, kind, from, to } => format!("{kind} {line} de {from} à {to}"),
        }
    }
}

fn pick<'a, T>(rng: &mut impl Rng, xs: &'a [T]) -> &'a T {
    &xs[rng.gen_range(0..xs.len())]
}

fn weighted<'a>(rng: &mut impl Rng, jobs: &'a HashMap<String, f32>) -> Option<&'a str> {
    let total: f32 = jobs.values().sum();
    if total <= 0.0 {
        return None;
    }
    let mut keys: Vec<_> = jobs.iter().collect();
    keys.sort_by(|a, b| a.0.cmp(b.0)); // stable order: the same seed gives the same job
    let mut r = rng.gen::<f32>() * total;
    for (k, w) in keys {
        r -= w;
        if r <= 0.0 {
            return Some(k);
        }
    }
    None
}

/// Why someone goes to a place of this kind at this hour.
fn purpose_for(rng: &mut impl Rng, kind: &str, place: &str, hour: u32, job: &str) -> String {
    let p = place;
    let options: Vec<String> = match kind {
        "amenity=restaurant" if hour < 16 => vec![format!("déjeuner à {p} avec des collègues"), format!("un déjeuner d'anniversaire à {p}"),
            format!("tester {p}, dont tout le monde parle"), format!("retrouver une amie à {p} pour déjeuner")],
        "amenity=restaurant" => vec![format!("dîner à {p}"), format!("un premier rendez-vous à {p}"), format!("un dîner de famille à {p}"),
            format!("fêter une bonne nouvelle à {p}")],
        "amenity=fast_food" => vec![format!("manger un morceau vite fait à {p}"), format!("chercher à emporter à {p}")],
        "amenity=cafe" => vec![format!("prendre un café à {p} avant de {}", if job == "étudiant" { "réviser" } else { "travailler" }),
            format!("travailler sur son ordi au {p}"), format!("retrouver quelqu'un au {p}")],
        "amenity=bar" | "amenity=pub" | "amenity=biergarten" => vec![format!("l'apéro au {p} avec des amis"), format!("un afterwork au {p}"),
            format!("voir un match au {p}"), format!("rejoindre des collègues au {p}")],
        "amenity=nightclub" => vec![format!("finir la soirée au {p}")],
        "shop=bakery" => vec![format!("acheter du pain chez {p}"), format!("chercher des viennoiseries chez {p}"), format!("acheter un gâteau nantais chez {p}")],
        "amenity=cinema" => vec![format!("voir un film au {p}"), format!("une séance en VO au {p}")],
        "amenity=theatre" | "amenity=arts_centre" => vec![format!("un spectacle à {p}"), format!("une répétition à {p}"), format!("une expo à {p}")],
        "tourism=museum" | "tourism=gallery" => vec![format!("visiter {p}"), format!("revoir une expo à {p}")],
        "tourism=attraction" | "tourism=viewpoint" => vec![format!("montrer {p} à de la famille de passage"), format!("prendre l'air vers {p}")],
        "tourism=hotel" => vec![format!("rentrer à son hôtel, {p}"), format!("déposer des bagages à {p}")],
        "leisure=sports_centre" | "leisure=stadium" => vec![format!("une séance de sport à {p}"), format!("l'entraînement à {p}")],
        "amenity=marketplace" => vec![format!("faire le marché ({p})")],
        "amenity=ice_cream" => vec![format!("une glace chez {p}")],
        k if k.starts_with("shop=") => vec![format!("des courses chez {p}"), format!("un cadeau à trouver chez {p}")],
        _ => vec![format!("passer à {p}")],
    };
    contract(pick(rng, &options))
}

/// Storylines: what is going on in their life right now (realistic, local, nothing tragic).
const STORIES: &[&str] = &[
    "cherche un appartement sur l'île de Nantes depuis trois mois et visite tout ce qui passe",
    "prépare un dossier pour faire classer la maison de sa grand-mère à {sub}",
    "répète pour le concert de sa chorale à la Folle Journée et chantonne tout le temps",
    "vient de se mettre au vélo et découvre la ville autrement (et ses carrefours)",
    "organise l'anniversaire surprise de sa sœur et cherche le bon resto sans qu'elle s'en doute",
    "a adopté un chien de refuge qui a peur des tramways",
    "écrit un roman qui se passe sur l'île Feydeau au XVIIIe siècle et se documente partout",
    "a monté une petite entreprise de livraison à vélo cargo à {quartier}",
    "fait partie d'une association qui replante des arbres le long de la Chézine",
    "prépare un marathon et s'entraîne au bord de l'Erdre tous les matins",
    "essaie de convaincre ses voisins de {sub} de faire un compost collectif",
    "va bientôt être grand-parent pour la première fois et ne parle que de ça",
    "apprend le breton en cours du soir et glisse des mots partout",
    "veut ouvrir une crêperie et goûte toutes celles de la ville pour « étudier la concurrence »",
    "vient d'arriver de Lyon pour un nouveau travail et découvre Nantes",
    "est bénévole au Voyage à Nantes l'été et connaît toutes les œuvres de la ligne verte",
    "restaure un vieux voilier au Chantier de l'Esclain le week-end",
    "collectionne les vieilles boîtes de biscuits LU et chine dans les vide-greniers",
    "prépare un concours et révise dans tous les cafés calmes de la ville",
    "monte un groupe de rock avec des copains et cherche une salle de répète",
    "organise la fête des voisins de {sub} et cherche des volontaires",
    "a gagné une place pour un tour sur le Grand Éléphant et le raconte à tout le monde",
    "fait une thèse sur l'histoire du port de Nantes et passe ses journées aux archives",
    "s'est mis au jardinage partagé dans le jardin de quartier de {sub}",
    "entraîne une équipe de foot de jeunes le samedi et rêve qu'un d'eux joue un jour aux Canaris",
    "vient de reprendre ses études à quarante ans et adore ça",
    "cherche des figurants pour un court-métrage tourné passage Pommeraye",
    "teste toutes les guinguettes du bord de l'Erdre pour un guide qu'il ne finira jamais",
    "a retrouvé un vieil ami d'enfance par hasard dans le tram la semaine dernière",
    "prépare son déménagement de {quartier} vers la campagne et hésite encore",
    "apprend à faire le beurre blanc de sa grand-mère, sans succès pour l'instant",
    "photographie toutes les façades penchées de l'île Feydeau pour une expo",
    "fait des visites guidées bénévoles du château pour les touristes",
    "milite pour une nouvelle passerelle piétonne sur la Loire et fait signer une pétition",
    "s'occupe d'une ruche sur un toit de {quartier}",
    "prépare un spectacle de théâtre de rue avec une petite compagnie",
];

const MOODS: &[&str] = &["de bonne humeur", "un peu fatigué", "pressé", "rêveur", "enjoué", "préoccupé (rien de grave)",
    "détendu", "bavard", "pensif", "ravi de la météo", "agacé par la météo"];

pub struct PersonaInput<'a> {
    pub seed: u32,
    pub hour: u32,
    pub home: Option<(usize, Option<usize>)>,
    pub place: Option<(&'a str, &'a str)>, // (kind, name)
    pub trip: Trip,
    pub march: Option<&'a str>,
}

pub fn persona(q: &Quartiers, i: PersonaInput) -> Persona {
    let mut rng = SmallRng::seed_from_u64(i.seed as u64 * 2654435761 + 17);
    let home = i.home.or_else(|| q.pick_home(&mut rng));
    let (qi, si) = home.unwrap_or((usize::MAX, None));
    let quart = q.quartiers.get(qi);
    let sub = quart.and_then(|qq| si.and_then(|s| qq.subs.get(s)));
    let age: u8 = match rng.gen_range(0..100) {
        0..=29 => rng.gen_range(18..27),
        30..=64 => rng.gen_range(27..60),
        _ => rng.gen_range(60..86),
    };
    let female = rng.gen_bool(0.5);
    let first = pick(&mut rng, match (female, age) {
        (true, 0..=30) => F_YOUNG, (false, 0..=30) => M_YOUNG,
        (true, 31..=59) => F_MID, (false, 31..=59) => M_MID,
        (true, _) => F_OLD, (false, _) => M_OLD,
    }).to_string();
    let last = pick(&mut rng, SURNAMES).to_string();
    let job = if age >= 64 {
        "retraité".to_string()
    } else {
        let j = quart.and_then(|qq| weighted(&mut rng, &qq.jobs)).unwrap_or("employé").to_string();
        if j == "retraité" && age < 58 { "employé".into() } else if j == "étudiant" && age > 30 { "enseignant".into() } else { j }
    };
    let archetype = if i.march.is_none() && rng.gen_bool(0.08) {
        let (a, b) = pick(&mut rng, ARCHETYPES);
        Some((a.to_string(), b.to_string()))
    } else if i.march.is_some() && rng.gen_bool(0.3) {
        Some((ARCHETYPES[0].0.to_string(), ARCHETYPES[0].1.to_string()))
    } else {
        None
    };
    let chatty = match (&archetype, age) {
        (Some(_), _) => rng.gen_range(0.6..1.0),
        (_, 64..) => rng.gen_range(0.4..1.0),
        _ => rng.gen::<f32>().powf(0.8),
    };
    let qname = quart.map(|q| q.name.clone()).unwrap_or_else(|| "Nantes".into());
    let sname = sub.map(|s| s.name.clone());
    let fill = |s: &str| s.replace("{quartier}", &qname).replace("{sub}", sname.as_deref().unwrap_or(&qname));
    let story = fill(pick(&mut rng, STORIES));
    let purpose = match (i.march, i.place) {
        (Some(m), _) => format!("défile dans la manifestation ({m})"),
        (_, Some((k, n))) => purpose_for(&mut rng, k, n, i.hour, &job),
        _ => "se promène".to_string(),
    };
    let mut tags: Vec<String> = sub.map(|s| s.tags.clone()).unwrap_or_default();
    tags.extend(quart.map(|q| q.tags.clone()).unwrap_or_default());
    Persona {
        first,
        last,
        age,
        female,
        job,
        archetype,
        quartier: qname,
        sub: sname,
        quartier_about: quart.map(|q| q.about.clone()).unwrap_or_default(),
        vibe: sub.and_then(|s| s.vibe.clone()),
        hood_tags: tags,
        chatty,
        mood: if chatty < 0.25 { "pressé".into() } else { pick(&mut rng, MOODS).to_string() },
        purpose,
        destination: i.place.map(|(_, n)| n.to_string()),
        story,
        trip: i.trip,
        march: i.march.map(|m| m.to_string()),
    }
}

// ---------------------------------------------------------------- trips on the timetable

/// One ride: board trip `trip` at its stop `k_on`, alight at `k_off`.
#[derive(Clone)]
pub struct Ride {
    pub trip: usize,
    pub k_on: usize,
    pub k_off: usize,
    pub dep: i64,
    pub arr: i64,
    pub on: [f32; 2],
    pub off: [f32; 2],
    pub on_name: String,
    pub off_name: String,
    pub line: String,
    pub kind: String,
}

pub struct TransitIndex {
    pub tt: Arc<Timetable>,
    /// per trip: per stop, its position
    pos: Vec<Vec<[f32; 2]>>,
    /// stop id -> (trip, k) sorted by departure
    at: HashMap<String, Vec<(usize, usize)>>,
    /// grid of 250 m cells -> stop ids (with a position)
    grid: HashMap<(i32, i32), Vec<(String, [f32; 2])>>,
    lines: HashMap<String, (String, u8)>,
}

const GRID: f32 = 250.0;
const WALK_V: f32 = 1.3;

/// Positions along a polyline at cumulative distances `s`.
fn along(p: &[[f32; 3]], s: &[f32]) -> Vec<[f32; 2]> {
    let mut out = Vec::with_capacity(s.len());
    let (mut i, mut acc) = (0usize, 0.0f32);
    for &d in s {
        while i + 1 < p.len() {
            let l = ((p[i + 1][0] - p[i][0]).powi(2) + (p[i + 1][1] - p[i][1]).powi(2)).sqrt();
            if acc + l >= d {
                let f = if l > 0.0 { (d - acc) / l } else { 0.0 };
                out.push([p[i][0] + (p[i + 1][0] - p[i][0]) * f, p[i][1] + (p[i + 1][1] - p[i][1]) * f]);
                break;
            }
            acc += l;
            i += 1;
        }
        if out.len() < s.len() && i + 1 >= p.len() {
            let q = p.last().copied().unwrap_or([0.0; 3]);
            out.push([q[0], q[1]]);
        }
    }
    out
}

#[derive(Deserialize)]
pub struct ShapeIn {
    pub p: Vec<[f32; 3]>,
    pub s: Vec<f32>,
}
#[derive(Deserialize)]
pub struct RouteIn {
    pub id: String,
    #[serde(default)]
    pub short: String,
    #[serde(rename = "type", default)]
    pub kind: u8,
}

impl TransitIndex {
    pub fn build(tt: Arc<Timetable>, shapes: &HashMap<String, ShapeIn>, routes: &[RouteIn]) -> TransitIndex {
        let mut pos = Vec::with_capacity(tt.trips.len());
        let mut at: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        let mut grid: HashMap<(i32, i32), Vec<(String, [f32; 2])>> = HashMap::new();
        let mut seen = std::collections::HashSet::new();
        for (ti, t) in tt.trips.iter().enumerate() {
            let pp = shapes.get(&t.shape).map(|s| along(&s.p, &s.s)).unwrap_or_default();
            if pp.len() == t.stop_ids.len() {
                for (k, sid) in t.stop_ids.iter().enumerate() {
                    at.entry(sid.clone()).or_default().push((ti, k));
                    if seen.insert(sid.clone()) {
                        let c = ((pp[k][0] / GRID).floor() as i32, (pp[k][1] / GRID).floor() as i32);
                        grid.entry(c).or_default().push((sid.clone(), pp[k]));
                    }
                }
            }
            pos.push(pp);
        }
        for v in at.values_mut() {
            v.sort_by_key(|&(t, k)| tt.trips[t].dep[k]);
        }
        let lines = routes.iter().map(|r| (r.id.clone(), (r.short.clone(), r.kind))).collect();
        TransitIndex { tt, pos, at, grid, lines }
    }

    fn near(&self, x: f32, y: f32, r: f32) -> Vec<(&str, [f32; 2], f32)> {
        let (cx, cy) = ((x / GRID).floor() as i32, (y / GRID).floor() as i32);
        let k = (r / GRID).ceil() as i32;
        let mut out = vec![];
        for i in -k..=k {
            for j in -k..=k {
                for (sid, p) in self.grid.get(&(cx + i, cy + j)).into_iter().flatten() {
                    let d = ((p[0] - x).powi(2) + (p[1] - y).powi(2)).sqrt();
                    if d <= r {
                        out.push((sid.as_str(), *p, d));
                    }
                }
            }
        }
        out
    }

    /// The quickest single ride from `o` to `d` leaving at `now` (walking to and from the stops), if it beats
    /// walking all the way by 5 minutes or more.
    pub fn plan(&self, o: [f32; 2], d: [f32; 2], now: i64) -> Option<Ride> {
        let direct = ((o[0] - d[0]).powi(2) + (o[1] - d[1]).powi(2)).sqrt() * 1.3 / WALK_V;
        let ends: Vec<_> = self.near(d[0], d[1], 600.0);
        if ends.is_empty() {
            return None;
        }
        let end_of: HashMap<&str, f32> = ends.iter().map(|e| (e.0, e.2)).collect();
        let mut best: Option<(f32, Ride)> = None;
        for (sid, sp, dist) in self.near(o[0], o[1], 600.0) {
            let ready = now + (dist * 1.3 / WALK_V) as i64 + 30;
            let Some(list) = self.at.get(sid) else { continue };
            let start = list.partition_point(|&(t, k)| self.tt.trips[t].dep[k] < ready);
            for &(t, k) in list[start..].iter().take(40) {
                let tr = &self.tt.trips[t];
                if tr.dep[k] > ready + 1800 {
                    break;
                }
                for j in k + 1..tr.stop_ids.len() {
                    let Some(&de) = end_of.get(tr.stop_ids[j].as_str()) else { continue };
                    let total = (tr.arr[j] - now) as f32 + de * 1.3 / WALK_V;
                    if best.as_ref().map_or(true, |b| total < b.0) {
                        let (short, kind) = self.lines.get(&tr.route).cloned().unwrap_or_default();
                        best = Some((total, Ride {
                            trip: t, k_on: k, k_off: j, dep: tr.dep[k], arr: tr.arr[j],
                            on: sp, off: self.pos[t][j],
                            on_name: tr.names[k].to_string(), off_name: tr.names[j].to_string(),
                            line: short,
                            kind: match kind { 0 => "tram", 4 => "navibus", _ => "bus" }.to_string(),
                        }));
                    }
                }
            }
        }
        best.filter(|b| b.0 + 300.0 < direct).map(|b| b.1)
    }
}
