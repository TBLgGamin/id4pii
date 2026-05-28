use std::fmt::Write as _;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use id4pii_core::{Category, PiiSpan, Rng, Vault, VaultEntry, anonymize_with_subs, deanonymize};

fn build_vault(n: usize) -> Vault {
    let mut entries = Vec::with_capacity(n);
    for i in 0..n {
        entries.push(VaultEntry {
            category: Category::PrivatePerson,
            real: format!("Real Person Number {i}"),
            fake: format!("Fake Surrogate Number {i}"),
        });
    }
    Vault { entries }
}

fn bench_deanonymize(c: &mut Criterion) {
    let vault = build_vault(1000);
    let mut text = String::new();
    for i in (0..1000).step_by(3) {
        let _ = write!(
            text,
            "please contact Fake Surrogate Number {i} regarding the open ticket today. "
        );
    }
    c.bench_function("deanonymize_1000_entries", |b| {
        b.iter(|| deanonymize(black_box(&text), black_box(&vault)));
    });
}

fn bench_anonymize(c: &mut Criterion) {
    let text = "Email Sarah Connor at sarah@skynet.com or call 555-0142 about the matter.";
    let spans = vec![
        PiiSpan {
            category: Category::PrivatePerson,
            start: 6,
            end: 18,
            text: "Sarah Connor".into(),
            score: 1.0,
        },
        PiiSpan {
            category: Category::PrivateEmail,
            start: 22,
            end: 38,
            text: "sarah@skynet.com".into(),
            score: 1.0,
        },
    ];
    c.bench_function("anonymize_with_subs", |b| {
        b.iter(|| {
            let mut rng = Rng::new(1);
            let mut vault = Vault::default();
            anonymize_with_subs(black_box(text), black_box(&spans), &mut rng, &mut vault)
        });
    });
}

criterion_group!(benches, bench_deanonymize, bench_anonymize);
criterion_main!(benches);
