use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::detector::PiiSpan;
use crate::labels::Category;

const FORENAMES_TSV: &str = include_str!("../assets/forenames.tsv");
const SURNAMES_TSV: &str = include_str!("../assets/surnames.tsv");

struct NamePool {
    names: Vec<&'static str>,
    #[allow(dead_code)] // reserved for future locale-aware filtering
    countries: Vec<&'static str>,
}

static FORENAMES_POOL: OnceLock<NamePool> = OnceLock::new();
static SURNAMES_POOL: OnceLock<NamePool> = OnceLock::new();

fn parse_pool(text: &'static str) -> NamePool {
    let mut names = Vec::with_capacity(2200);
    let mut countries = Vec::with_capacity(2200);
    for line in text.lines() {
        if let Some((country, name)) = line.split_once('\t') {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            countries.push(country);
            names.push(name);
        }
    }
    NamePool { names, countries }
}

fn forenames() -> &'static NamePool {
    FORENAMES_POOL.get_or_init(|| parse_pool(FORENAMES_TSV))
}

fn surnames() -> &'static NamePool {
    SURNAMES_POOL.get_or_init(|| parse_pool(SURNAMES_TSV))
}

/// Eagerly parse the embedded name pools. Safe to call from any thread, idempotent. Useful
/// from a background warm-up thread at app startup so the first anonymize doesn't pay the
/// ~few-ms parse cost on the user-facing path.
pub fn warm_up_pools() {
    let _ = forenames();
    let _ = surnames();
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultEntry {
    pub category: Category,
    pub real: String,
    pub fake: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Vault {
    pub entries: Vec<VaultEntry>,
}

impl Vault {
    pub fn surrogate_for(&mut self, category: Category, real: &str, rng: &mut Rng) -> String {
        if let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.category == category && entry.real == real)
        {
            return entry.fake.clone();
        }
        let fake = unique_fake(category, rng, self);
        self.entries.push(VaultEntry {
            category,
            real: real.to_string(),
            fake: fake.clone(),
        });
        fake
    }
}

#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x2545_F491_4F6C_DD1D,
        }
    }

    #[must_use]
    pub fn from_entropy() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x1234_5678, |elapsed| elapsed.as_nanos() as u64);
        Self::new(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            0
        } else {
            self.next_u64() % bound
        }
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len() as u64) as usize]
    }

    fn digit(&mut self) -> char {
        char::from(b'0' + self.below(10) as u8)
    }

    fn alphanumeric(&mut self) -> char {
        const POOL: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        char::from(POOL[self.below(POOL.len() as u64) as usize])
    }
}

const STREET_NAMES: &[&str] = &[
    "Baker",
    "Evergreen",
    "Mockingbird",
    "Riverside",
    "Sunset",
    "Maple",
    "Oak",
    "Cedar",
    "Willow",
    "Birch",
    "Lakeshore",
    "Hawthorn",
    "Bagshot",
    "Privet",
    "Spooner",
    "Elm",
    "Cherry",
    "Aspen",
    "Linden",
    "Juniper",
    "Sycamore",
    "Magnolia",
    "Chestnut",
    "Holly",
    "Marigold",
    "Brookline",
    "Foxglove",
    "Ironwood",
    "Wisteria",
    "Greenfield",
    "Hillcrest",
    "Meadowbrook",
];

const STREET_TYPES: &[&str] = &[
    "Street",
    "Avenue",
    "Road",
    "Lane",
    "Drive",
    "Court",
    "Way",
    "Boulevard",
    "Terrace",
    "Place",
];

const DOMAINS: &[&str] = &[
    "gmail.com",
    "outlook.com",
    "hotmail.com",
    "yahoo.com",
    "icloud.com",
    "live.com",
    "proton.me",
];

const URL_WORDS: &[&str] = &[
    "holodeck",
    "mainframe",
    "rivendell",
    "matrix",
    "tardis",
    "hyrule",
    "moria",
    "tatooine",
    "gibson",
    "zion",
    "archive",
    "vault",
    "portal",
    "console",
    "gateway",
    "sandbox",
    "nexus",
    "atlas",
    "beacon",
    "harbor",
    "orbit",
    "relay",
    "summit",
    "haven",
];

#[must_use]
pub fn anonymize(text: &str, spans: &[PiiSpan], rng: &mut Rng) -> (String, Vault) {
    let mut vault = Vault::default();
    let result = anonymize_into(text, spans, rng, &mut vault);
    (result, vault)
}

#[must_use]
pub fn anonymize_into(text: &str, spans: &[PiiSpan], rng: &mut Rng, vault: &mut Vault) -> String {
    let (result, _) = anonymize_with_subs(text, spans, rng, vault);
    result
}

/// Like [`anonymize_into`], but also returns the list of `(real, fake)` substitutions in
/// document order. Useful when the caller wants to apply per-substring edits to a rich-text
/// surface (preserving formatting) instead of overwriting the whole text.
#[must_use]
pub fn anonymize_with_subs(
    text: &str,
    spans: &[PiiSpan],
    rng: &mut Rng,
    vault: &mut Vault,
) -> (String, Vec<(String, String)>) {
    let mut ordered: Vec<&PiiSpan> = spans.iter().collect();
    ordered.sort_by_key(|span| span.start);

    let mut result = String::with_capacity(text.len());
    let mut subs: Vec<(String, String)> = Vec::new();
    let mut cursor = 0;
    for span in ordered {
        if span.start < cursor || span.end > text.len() || span.start > span.end {
            continue;
        }
        if let Some(prefix) = text.get(cursor..span.start) {
            result.push_str(prefix);
        }
        let fake = vault.surrogate_for(span.category, &span.text, rng);
        result.push_str(&fake);
        subs.push((span.text.clone(), fake));
        cursor = span.end;
    }
    if let Some(rest) = text.get(cursor..) {
        result.push_str(rest);
    }
    (result, subs)
}

#[must_use]
pub fn deanonymize(text: &str, vault: &Vault) -> String {
    let mut pairs: Vec<(&str, &str)> = vault
        .entries
        .iter()
        .map(|entry| (entry.fake.as_str(), entry.real.as_str()))
        .filter(|(fake, _)| !fake.is_empty())
        .collect();
    pairs.sort_by_key(|(fake, _)| std::cmp::Reverse(fake.len()));

    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let mut matched = false;
        for (fake, real) in &pairs {
            if let Some(stripped) = rest.strip_prefix(fake) {
                result.push_str(real);
                rest = stripped;
                matched = true;
                break;
            }
        }
        if !matched {
            let mut chars = rest.chars();
            if let Some(character) = chars.next() {
                result.push(character);
                rest = chars.as_str();
            }
        }
    }
    result
}

fn unique_fake(category: Category, rng: &mut Rng, vault: &Vault) -> String {
    for _ in 0..64 {
        let candidate = generate_fake(category, rng);
        if !vault.entries.iter().any(|entry| entry.fake == candidate) {
            return candidate;
        }
    }
    let mut candidate = generate_fake(category, rng);
    candidate.push('-');
    candidate.push(rng.digit());
    candidate.push(rng.digit());
    candidate
}

fn sanitize_local_part(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn generate_fake(category: Category, rng: &mut Rng) -> String {
    match category {
        Category::PrivatePerson => {
            let f = forenames();
            let s = surnames();
            let fi = rng.below(f.names.len() as u64) as usize;
            let si = rng.below(s.names.len() as u64) as usize;
            format!("{} {}", f.names[fi], s.names[si])
        }
        Category::PrivateEmail => {
            let f = forenames();
            let s = surnames();
            let first = sanitize_local_part(f.names[rng.below(f.names.len() as u64) as usize]);
            let last = sanitize_local_part(s.names[rng.below(s.names.len() as u64) as usize]);
            let suffix = rng.below(100);
            format!("{first}.{last}{suffix:02}@{}", rng.pick(DOMAINS))
        }
        Category::PrivatePhone => {
            let area = 200 + rng.below(800);
            let exchange = 200 + rng.below(800);
            let line = rng.below(10_000);
            format!("({area:03}) {exchange:03}-{line:04}")
        }
        Category::PrivateAddress => {
            let number = 1 + rng.below(9899);
            format!(
                "{number} {} {}",
                rng.pick(STREET_NAMES),
                rng.pick(STREET_TYPES)
            )
        }
        Category::PrivateDate => {
            let year = 1970 + rng.below(50);
            let month = 1 + rng.below(12);
            let day = 1 + rng.below(28);
            format!("{year:04}-{month:02}-{day:02}")
        }
        Category::PrivateUrl => {
            format!(
                "https://{}/{}-{}{}",
                rng.pick(DOMAINS),
                rng.pick(URL_WORDS),
                rng.digit(),
                rng.digit()
            )
        }
        Category::AccountNumber => {
            let mut digits = String::with_capacity(10);
            for _ in 0..10 {
                digits.push(rng.digit());
            }
            digits
        }
        Category::Secret => {
            let mut token = String::from("sk-fake-");
            for _ in 0..16 {
                token.push(rng.alphanumeric());
            }
            token
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(category: Category, start: usize, end: usize, text: &str) -> PiiSpan {
        PiiSpan {
            category,
            start,
            end,
            text: text.to_string(),
            score: 1.0,
        }
    }

    #[test]
    fn round_trip_restores_original() {
        let text = "Email John Smith at john@corp.com today";
        let spans = vec![
            span(Category::PrivatePerson, 6, 16, "John Smith"),
            span(Category::PrivateEmail, 20, 33, "john@corp.com"),
        ];
        let mut rng = Rng::new(42);
        let (anonymized, vault) = anonymize(text, &spans, &mut rng);
        assert_ne!(anonymized, text);
        assert!(!anonymized.contains("John Smith"));
        assert_eq!(deanonymize(&anonymized, &vault), text);
    }

    #[test]
    fn repeated_value_maps_to_one_surrogate() {
        let text = "John told John a secret";
        let spans = vec![
            span(Category::PrivatePerson, 0, 4, "John"),
            span(Category::PrivatePerson, 10, 14, "John"),
        ];
        let mut rng = Rng::new(7);
        let (anonymized, vault) = anonymize(text, &spans, &mut rng);
        assert_eq!(vault.entries.len(), 1);
        let surrogate = &vault.entries[0].fake;
        assert_eq!(anonymized.matches(surrogate.as_str()).count(), 2);
    }

    #[test]
    fn distinct_values_get_distinct_surrogates() {
        let text = "Alice and Bob";
        let spans = vec![
            span(Category::PrivatePerson, 0, 5, "Alice"),
            span(Category::PrivatePerson, 10, 13, "Bob"),
        ];
        let mut rng = Rng::new(99);
        let (_, vault) = anonymize(text, &spans, &mut rng);
        assert_eq!(vault.entries.len(), 2);
        assert_ne!(vault.entries[0].fake, vault.entries[1].fake);
    }
}
