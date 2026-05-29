#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]

use std::fmt::Write as _;
use std::hint::black_box;
use std::path::PathBuf;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use id4pii_core::eval::{Example, load_tsv};
use id4pii_core::{
    Category, PiiSpan, Rng, Vault, VaultEntry, anonymize_with_subs, deanonymize, regex_scan,
};

fn dataset_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/pii_dataset.tsv")
}

fn load() -> Vec<Example> {
    load_tsv(&dataset_path()).expect("load benchmark corpus")
}

fn spans_of(ex: &Example) -> Vec<PiiSpan> {
    ex.labels
        .iter()
        .filter_map(|l| {
            l.category.map(|category| PiiSpan {
                category,
                start: l.start,
                end: l.end,
                text: ex.text[l.start..l.end].to_string(),
                score: 1.0,
            })
        })
        .collect()
}

fn bench_parse(c: &mut Criterion) {
    let bytes = std::fs::metadata(dataset_path()).map_or(0, |m| m.len());
    let path = dataset_path();
    let mut group = c.benchmark_group("parse");
    group.throughput(Throughput::Bytes(bytes));
    group.bench_function("load_tsv", |b| {
        b.iter(|| load_tsv(black_box(&path)).unwrap());
    });
    group.finish();
}

fn bench_detect_regex(c: &mut Criterion) {
    let examples = load();
    let total_bytes: u64 = examples.iter().map(|e| e.text.len() as u64).sum();
    let mut group = c.benchmark_group("detect_regex");
    group.throughput(Throughput::Bytes(total_bytes));
    group.bench_function("regex_scan_corpus", |b| {
        b.iter(|| {
            let mut found = 0usize;
            for ex in &examples {
                found += regex_scan(black_box(&ex.text)).len();
            }
            black_box(found)
        });
    });
    group.finish();
}

fn bench_anonymize(c: &mut Criterion) {
    let examples = load();
    let spans: Vec<Vec<PiiSpan>> = examples.iter().map(spans_of).collect();
    let mut group = c.benchmark_group("anonymize");
    group.throughput(Throughput::Elements(examples.len() as u64));
    group.bench_function("anonymize_corpus", |b| {
        b.iter(|| {
            let mut rng = Rng::new(1);
            let mut vault = Vault::default();
            for (ex, sp) in examples.iter().zip(&spans) {
                let _ =
                    anonymize_with_subs(black_box(&ex.text), black_box(sp), &mut rng, &mut vault);
            }
        });
    });
    group.finish();
}

fn bench_deanonymize(c: &mut Criterion) {
    let examples = load();

    let mut rng = Rng::new(1);
    let mut vault = Vault::default();
    let mut anon_texts = Vec::with_capacity(examples.len());
    for ex in &examples {
        let (anon, _) = anonymize_with_subs(&ex.text, &spans_of(ex), &mut rng, &mut vault);
        anon_texts.push(anon);
    }
    let total_bytes: u64 = anon_texts.iter().map(|t| t.len() as u64).sum();

    let mut group = c.benchmark_group("deanonymize");
    group.throughput(Throughput::Bytes(total_bytes));
    group.bench_function("deanonymize_corpus", |b| {
        b.iter(|| {
            for t in &anon_texts {
                black_box(deanonymize(black_box(t), black_box(&vault)));
            }
        });
    });
    group.finish();
}

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

fn bench_scaling(c: &mut Criterion) {
    let vault = build_vault(1000);
    let mut text = String::new();
    for i in (0..1000).step_by(3) {
        let _ = write!(
            text,
            "please contact Fake Surrogate Number {i} regarding the open ticket today. "
        );
    }
    let mut group = c.benchmark_group("scaling");
    group.bench_function("deanonymize_1000_entries", |b| {
        b.iter(|| deanonymize(black_box(&text), black_box(&vault)));
    });

    let small_text = "Email Sarah Connor at sarah@skynet.com or call 555-0142 about the matter.";
    let small_spans = vec![
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
    group.bench_function("anonymize_with_subs", |b| {
        b.iter(|| {
            let mut rng = Rng::new(1);
            let mut vault = Vault::default();
            anonymize_with_subs(
                black_box(small_text),
                black_box(&small_spans),
                &mut rng,
                &mut vault,
            )
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_parse,
    bench_detect_regex,
    bench_anonymize,
    bench_deanonymize,
    bench_scaling
);
criterion_main!(benches);
