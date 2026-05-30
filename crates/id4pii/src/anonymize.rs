use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::detect::PiiSpan;
use crate::labels::Category;

const FORENAMES_TSV: &str = include_str!("../assets/forenames.tsv");
const SURNAMES_TSV: &str = include_str!("../assets/surnames.tsv");

struct NamePool {
    names: Vec<&'static str>,
}

static FORENAMES_POOL: OnceLock<NamePool> = OnceLock::new();
static SURNAMES_POOL: OnceLock<NamePool> = OnceLock::new();

fn parse_pool(text: &'static str) -> NamePool {
    let mut names = Vec::with_capacity(2200);
    for line in text.lines() {
        if let Some((_, name)) = line.split_once('\t') {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            names.push(name);
        }
    }
    NamePool { names }
}

fn forenames() -> &'static NamePool {
    FORENAMES_POOL.get_or_init(|| parse_pool(FORENAMES_TSV))
}

fn surnames() -> &'static NamePool {
    SURNAMES_POOL.get_or_init(|| parse_pool(SURNAMES_TSV))
}

pub fn warm_up_pools() {
    let _ = forenames();
    let _ = surnames();
}

pub trait SurrogateStore {
    fn surrogate_for(&mut self, category: Category, real: &str, rng: &mut Rng) -> String;
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
        let fake = unique_fake_checked(category, rng, |candidate| {
            self.entries.iter().any(|entry| entry.fake == candidate)
        });
        self.entries.push(VaultEntry {
            category,
            real: real.to_string(),
            fake: fake.clone(),
        });
        fake
    }

    pub fn enforce_cap(&mut self, max_entries: usize) -> usize {
        if max_entries == 0 || self.entries.len() <= max_entries {
            return 0;
        }
        let evicted = self.entries.len() - max_entries;
        self.entries.drain(0..evicted);
        evicted
    }
}

impl SurrogateStore for Vault {
    fn surrogate_for(&mut self, category: Category, real: &str, rng: &mut Rng) -> String {
        Vault::surrogate_for(self, category, real, rng)
    }
}

#[derive(Debug, Clone, Default)]
pub struct IndexedVault {
    vault: Vault,
    by_real: HashMap<Category, HashMap<String, usize>>,
    fakes: HashSet<String>,
}

impl IndexedVault {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn from_vault(vault: Vault) -> Self {
        let mut by_real: HashMap<Category, HashMap<String, usize>> = HashMap::new();
        let mut fakes = HashSet::with_capacity(vault.entries.len());
        for (index, entry) in vault.entries.iter().enumerate() {
            by_real
                .entry(entry.category)
                .or_default()
                .insert(entry.real.clone(), index);
            fakes.insert(entry.fake.clone());
        }
        Self {
            vault,
            by_real,
            fakes,
        }
    }

    #[must_use]
    pub fn into_vault(self) -> Vault {
        self.vault
    }

    #[must_use]
    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.vault.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.vault.entries.is_empty()
    }
}

impl SurrogateStore for IndexedVault {
    fn surrogate_for(&mut self, category: Category, real: &str, rng: &mut Rng) -> String {
        if let Some(&index) = self.by_real.get(&category).and_then(|map| map.get(real)) {
            return self.vault.entries[index].fake.clone();
        }
        let fake = unique_fake_checked(category, rng, |candidate| self.fakes.contains(candidate));
        let index = self.vault.entries.len();
        self.vault.entries.push(VaultEntry {
            category,
            real: real.to_string(),
            fake: fake.clone(),
        });
        self.by_real
            .entry(category)
            .or_default()
            .insert(real.to_string(), index);
        self.fakes.insert(fake.clone());
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
pub fn anonymize_into<S: SurrogateStore>(
    text: &str,
    spans: &[PiiSpan],
    rng: &mut Rng,
    store: &mut S,
) -> String {
    let (result, _) = anonymize_with_subs(text, spans, rng, store);
    result
}

#[derive(Debug, Clone)]
pub struct Placement {
    pub start: usize,
    pub end: usize,
    pub surrogate: String,
}

#[must_use]
pub fn anonymize_placements<S: SurrogateStore>(
    text: &str,
    spans: &[PiiSpan],
    rng: &mut Rng,
    store: &mut S,
) -> (Vec<Placement>, Vec<(String, String)>) {
    let mut ordered: Vec<&PiiSpan> = spans.iter().collect();
    ordered.sort_by_key(|span| span.start);

    let mut placements: Vec<Placement> = Vec::new();
    let mut subs: Vec<(String, String)> = Vec::new();
    let mut cursor = 0;
    for span in ordered {
        if span.start < cursor || span.end > text.len() || span.start > span.end {
            continue;
        }
        let fake = store.surrogate_for(span.category, &span.text, rng);
        placements.push(Placement {
            start: span.start,
            end: span.end,
            surrogate: fake.clone(),
        });
        subs.push((span.text.clone(), fake));
        cursor = span.end;
    }
    (placements, subs)
}

#[must_use]
pub fn apply_placements(text: &str, placements: &[Placement]) -> String {
    let mut result = String::with_capacity(text.len());
    let mut cursor = 0;
    for placement in placements {
        if let Some(prefix) = text.get(cursor..placement.start) {
            result.push_str(prefix);
        }
        result.push_str(&placement.surrogate);
        cursor = placement.end;
    }
    if let Some(rest) = text.get(cursor..) {
        result.push_str(rest);
    }
    result
}

#[must_use]
pub fn anonymize_with_subs<S: SurrogateStore>(
    text: &str,
    spans: &[PiiSpan],
    rng: &mut Rng,
    store: &mut S,
) -> (String, Vec<(String, String)>) {
    let (placements, subs) = anonymize_placements(text, spans, rng, store);
    (apply_placements(text, &placements), subs)
}

#[must_use]
pub fn deanonymize(text: &str, vault: &Vault) -> String {
    let mut buckets: Vec<Vec<(&str, &str)>> = Vec::new();
    buckets.resize_with(256, Vec::new);
    for entry in &vault.entries {
        let fake = entry.fake.as_str();
        if let Some(&first) = fake.as_bytes().first() {
            buckets[first as usize].push((fake, entry.real.as_str()));
        }
    }

    for candidates in &mut buckets {
        if candidates.len() > 1 {
            candidates.sort_by_key(|(fake, _)| std::cmp::Reverse(fake.len()));
        }
    }

    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(&first) = rest.as_bytes().first() {
        let matched = buckets[first as usize]
            .iter()
            .find_map(|(fake, real)| rest.strip_prefix(fake).map(|stripped| (*real, stripped)));
        if let Some((real, stripped)) = matched {
            result.push_str(real);
            rest = stripped;
        } else {
            let mut chars = rest.chars();
            if let Some(character) = chars.next() {
                result.push(character);
                rest = chars.as_str();
            } else {
                break;
            }
        }
    }
    result
}

fn unique_fake_checked(
    category: Category,
    rng: &mut Rng,
    is_taken: impl Fn(&str) -> bool,
) -> String {
    for _ in 0..64 {
        let candidate = generate_fake(category, rng);
        if !is_taken(&candidate) {
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
        .filter(char::is_ascii_alphanumeric)
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

    fn entry(real: &str, fake: &str) -> VaultEntry {
        VaultEntry {
            category: Category::PrivatePerson,
            real: real.to_string(),
            fake: fake.to_string(),
        }
    }

    #[test]
    fn deanonymize_prefers_longest_surrogate_sharing_a_prefix() {
        let vault = Vault {
            entries: vec![entry("Alice", "Bob"), entry("Carol", "Bobby")],
        };
        assert_eq!(deanonymize("Bobby and Bob", &vault), "Carol and Alice");
    }

    #[test]
    fn enforce_cap_evicts_oldest_and_is_unbounded_at_zero() {
        let mut vault = Vault {
            entries: vec![entry("Aaa", "f1"), entry("Bbb", "f2"), entry("Ccc", "f3")],
        };
        assert_eq!(vault.enforce_cap(0), 0);
        assert_eq!(vault.entries.len(), 3);
        assert_eq!(vault.enforce_cap(5), 0);
        assert_eq!(vault.enforce_cap(2), 1);
        assert_eq!(vault.entries.len(), 2);
        assert_eq!(vault.entries[0].real, "Bbb");
        assert_eq!(vault.entries[1].real, "Ccc");
    }

    #[test]
    fn indexed_vault_matches_plain_vault_surrogates() {
        let calls = [
            (Category::PrivatePerson, "Alice"),
            (Category::PrivateEmail, "a@b.com"),
            (Category::PrivatePerson, "Alice"),
            (Category::PrivatePerson, "Bob"),
            (Category::PrivateEmail, "a@b.com"),
            (Category::AccountNumber, "12345"),
        ];
        let mut plain = Vault::default();
        let mut plain_rng = Rng::new(123);
        let mut indexed = IndexedVault::new();
        let mut indexed_rng = Rng::new(123);
        for (category, real) in calls {
            let from_plain = plain.surrogate_for(category, real, &mut plain_rng);
            let from_indexed = indexed.surrogate_for(category, real, &mut indexed_rng);
            assert_eq!(from_plain, from_indexed);
        }
        assert_eq!(plain.entries.len(), indexed.len());
        assert_eq!(indexed.len(), 4);
    }

    #[test]
    fn indexed_vault_round_trips_through_deanonymize() {
        let text = "Email John Smith at john@corp.com today";
        let spans = vec![
            span(Category::PrivatePerson, 6, 16, "John Smith"),
            span(Category::PrivateEmail, 20, 33, "john@corp.com"),
        ];
        let mut rng = Rng::new(42);
        let mut indexed = IndexedVault::new();
        let anonymized = anonymize_into(text, &spans, &mut rng, &mut indexed);
        assert_ne!(anonymized, text);
        let vault = indexed.into_vault();
        assert_eq!(deanonymize(&anonymized, &vault), text);
    }

    #[test]
    fn deanonymize_restores_midtext_and_skips_empty_fakes() {
        let vault = Vault {
            entries: vec![entry("real@corp.com", "fake@example.com"), entry("Zed", "")],
        };
        assert_eq!(
            deanonymize("mail fake@example.com now", &vault),
            "mail real@corp.com now"
        );
    }
}
