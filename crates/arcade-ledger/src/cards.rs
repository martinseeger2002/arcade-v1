//! App cards (the original's "game cards", games.py 14-141; docs/compat/APPS.md): the JSON beside
//! an arcade inscription of an HTML page, under "game" (preferred) or "app". Every value is
//! cleaned and cut; nothing is trusted.

use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Card {
    pub name: String,
    pub description: String,
    pub version: String,
    pub players: String,
    pub genre: String,
    /// The App Store category (V1.0; the original has none): a short lowercase name, e.g.
    /// "games", "node-modules". A card without one is "games" ("game" cards) or "apps".
    pub category: String,
    pub multiplayer: bool,
    pub wallet: bool,
    pub media: bool,
    pub pictures: bool,
    pub notify: bool,
    pub contacts: bool,
    pub groups: bool,
    pub family: String,
    pub cover: String,
    pub play: String,
    pub tdeck: String,
    pub tdeck_icon: String,
    /// Seconds an app may stay minimized; None = no limit.
    pub minimized_timeout: Option<u64>,
}

fn text(v: &Value, limit: usize) -> String {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return String::new(), // bool, null, lists, objects: refused
    };
    s.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(limit).collect()
}

fn hex64(v: &Value) -> String {
    let s = v.as_str().unwrap_or("").to_ascii_lowercase();
    if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) { s } else { String::new() }
}

fn minimized(v: &Value) -> Option<u64> {
    let n = match v {
        Value::Object(o) => o.get("timeout").cloned().unwrap_or(Value::Null),
        other => other.clone(),
    };
    let n = match n {
        Value::Number(x) => x.as_f64(),
        _ => None,
    };
    match n {
        None => Some(900),
        Some(x) if !x.is_finite() || x < 0.0 => Some(900),
        Some(x) if x == 0.0 => None,
        Some(x) => Some((x as u64).clamp(1, 86_400)),
    }
}

/// The card in an inscription's JSON, or None when it isn't one.
/// A category name: lowercase letters, digits and inner dashes, up to 24 ("Node Modules" reads as
/// "node-modules").
fn category(v: &Value) -> Option<String> {
    let s = v.as_str()?.trim().to_lowercase().split_whitespace().collect::<Vec<_>>().join("-");
    let ok = (1..=24).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-') && !s.ends_with('-') && !s.contains("--");
    ok.then_some(s)
}

pub fn parse(json_text: &str) -> Option<Card> {
    let v: Value = serde_json::from_str(json_text).ok()?;
    let (top, d) = ["game", "app"].iter().find_map(|m| v.get(*m).filter(|x| x.is_object()).map(|x| (*m, x)))?;
    let name = text(&d["name"], 60);
    if name.is_empty() {
        return None;
    }
    let family = d["family"].as_str().filter(|f| (1..=64).contains(&f.len()) && f.bytes().all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))).unwrap_or("").to_string();
    let icon = d["tdeck_icon"].as_str().filter(|f| (1..=16).contains(&f.len()) && f.bytes().all(|b| b.is_ascii_lowercase())).unwrap_or("").to_string();
    Some(Card {
        name,
        description: text(&d["description"], 280),
        version: text(&d["version"], 20),
        players: text(&d["players"], 20),
        genre: text(&d["genre"], 24),
        category: category(&d["category"]).unwrap_or_else(|| if top == "game" { "games".into() } else { "apps".into() }),
        multiplayer: d["multiplayer"] == true,
        wallet: d["wallet"] == true,
        media: d["media"] == true,
        pictures: d["pictures"] == true,
        notify: d["notify"] == true,
        contacts: d["contacts"] == true,
        groups: d["groups"] == true,
        family,
        cover: hex64(&d["cover"]),
        play: hex64(&d["play"]),
        tdeck: hex64(&d["tdeck"]),
        tdeck_icon: icon,
        minimized_timeout: minimized(d.get("minimized").unwrap_or(&Value::Null)),
    })
}

impl Card {
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name, "description": self.description, "version": self.version, "players": self.players,
            "genre": self.genre, "category": self.category, "multiplayer": self.multiplayer, "wallet": self.wallet, "media": self.media,
            "pictures": self.pictures, "notify": self.notify, "contacts": self.contacts, "groups": self.groups,
            "family": self.family, "cover": self.cover, "play": self.play, "tdeck": self.tdeck, "tdeck_icon": self.tdeck_icon,
            "minimized_timeout": self.minimized_timeout,
        })
    }

    /// The same app across versions: its name, casefolded.
    pub fn key(&self) -> String {
        self.name.to_lowercase()
    }
}

/// The original card watch's spelling(): lookalike letters folded, so names that only look
/// alike compare equal (cardwatch.py 83-98).
pub fn spelling(s: &str) -> String {
    let mut out = String::new();
    let lower = s.to_lowercase();
    let mut chars: Vec<char> = Vec::new();
    for c in lower.chars() {
        // Strip accents for the common Latin letters.
        let c = match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' => 'a',
            'è' | 'é' | 'ê' | 'ë' | 'ē' => 'e',
            'ì' | 'í' | 'î' | 'ï' | 'ī' => 'i',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' => 'o',
            'ù' | 'ú' | 'û' | 'ü' | 'ū' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            'ý' | 'ÿ' => 'y',
            // Cyrillic and Greek lookalikes.
            'а' | 'α' => 'a',
            'в' | 'β' => 'b',
            'с' | 'ς' => 'c',
            'е' | 'ε' => 'e',
            'н' | 'η' => 'h',
            'і' | 'ι' => 'i',
            'ј' => 'j',
            'к' | 'κ' => 'k',
            'м' | 'μ' => 'm',
            'о' | 'ο' => 'o',
            'р' | 'ρ' => 'p',
            'ѕ' => 's',
            'т' | 'τ' => 't',
            'у' | 'υ' => 'y',
            'х' | 'χ' => 'x',
            'ν' => 'v',
            other => other,
        };
        chars.push(c);
    }
    let joined: String = chars.into_iter().collect();
    let joined = joined.replace("rn", "m").replace("vv", "w").replace("cl", "d");
    for c in joined.chars() {
        let c = match c {
            '0' => 'o',
            '1' | 'i' | 'j' | '!' | '|' => 'l',
            '3' => 'e',
            '4' | '@' => 'a',
            '5' | '$' => 's',
            '7' => 't',
            '8' => 'b',
            '6' | '9' => 'g',
            '2' => 'z',
            c => c,
        };
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if out.chars().last() != Some(c) {
                out.push(c);
            }
        }
    }
    out
}

fn levenshtein(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1]));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Does one card look like another (name or family, crosswise)? Parts shorter than 4 are ignored.
pub fn looks_like(a: &Card, b: &Card) -> bool {
    if !a.cover.is_empty() && a.cover == b.cover {
        return true;
    }
    let parts = |c: &Card| [spelling(&c.name), spelling(&c.family)].into_iter().filter(|p| p.len() >= 4).collect::<Vec<_>>();
    for x in parts(a) {
        for y in parts(b) {
            if x == y || (y.len() >= 5 && x.contains(&y)) || (x.len() >= 5 && y.contains(&x)) {
                return true;
            }
            let d = levenshtein(&x, &y);
            if d <= if x.len().min(y.len()) < 7 { 1 } else { 2 } {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors() {
        let c = parse(r#"{"app":{"name":"  A   b "}}"#).unwrap();
        assert_eq!((c.name.as_str(), c.minimized_timeout), ("A b", Some(900)));
        assert_eq!(parse(r#"{"app":{"name":"x","minimized":0}}"#).unwrap().minimized_timeout, None);
        assert_eq!(parse(r#"{"app":{"name":"x","minimized":{"timeout":999999}}}"#).unwrap().minimized_timeout, Some(86400));
        assert!(!parse(r#"{"app":{"name":"x","multiplayer":"true"}}"#).unwrap().multiplayer);
        assert!(parse(r#"{"app":{"name":true}}"#).is_none());
        assert_eq!(parse(r#"{"game":{"name":"G"},"app":{"name":"A"}}"#).unwrap().name, "G");
        let up = "ABCDEF".repeat(10) + "ABCD";
        assert_eq!(parse(&format!(r#"{{"app":{{"name":"x","cover":"{up}"}}}}"#)).unwrap().cover, up.to_lowercase());
        assert_eq!(parse(r#"{"app":{"name":"x","family":"has space"}}"#).unwrap().family, "");
        assert_eq!(parse(r#"{"game":{"name":"G"}}"#).unwrap().category, "games");
        assert_eq!(parse(r#"{"app":{"name":"A"}}"#).unwrap().category, "apps");
        assert_eq!(parse(r#"{"app":{"name":"M","category":"Node Modules"}}"#).unwrap().category, "node-modules");
        assert_eq!(parse(r#"{"app":{"name":"T","category":"themes"}}"#).unwrap().category, "themes");
        for bad in [r#""-x""#, r#""a--b""#, r#""<b>""#, r#""abcdefghijklmnopqrstuvwxyz""#, "7", r#""""#] {
            assert_eq!(parse(&format!(r#"{{"app":{{"name":"B","category":{bad}}}}}"#)).unwrap().category, "apps", "{bad}");
        }
        let riffs = parse(r#"{"app":{"name":"Riffs","description":"Group chats","version":"2.0","players":"2-20","multiplayer":true,"family":"riffs","genre":"Social","media":true,"groups":true,"notify":true,"pictures":true,"cover":"c1eaa01180715c7829185dc8020a34153045bae05c912874b231bb24934ff32c"}}"#).unwrap();
        assert!(riffs.multiplayer && riffs.media && riffs.groups && riffs.notify && riffs.pictures && !riffs.wallet);
        assert_eq!((riffs.family.as_str(), riffs.version.as_str()), ("riffs", "2.0"));
    }

    #[test]
    fn lookalikes() {
        let c = |n: &str| Card { name: n.into(), ..Default::default() };
        assert!(looks_like(&c("Riffs"), &c("R1ffs")));
        assert!(looks_like(&c("Pocket Tide"), &c("Pocket Tlde Super")));
        assert!(looks_like(&c("Tank Simulator"), &c("Tank Sirnulator")));
        assert!(!looks_like(&c("Riffs"), &c("Browser")));
        assert!(!looks_like(&c("Go"), &c("Do")));
        assert_eq!(spelling("Rîffs"), spelling("riffs"));
        assert_eq!(spelling("Rîffs!"), "rlfsl");
    }
}
