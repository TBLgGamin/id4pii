#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use id4pii_core::eval::{evaluate, load_tsv};
use id4pii_core::{Category, regex_scan};

fn corpus() -> Vec<id4pii_core::eval::Example> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/pii_dataset.tsv");
    load_tsv(&path).expect("load committed corpus")
}

#[test]
fn corpus_loads() {
    let examples = corpus();
    assert_eq!(examples.len(), 1500, "expected 1500 labelled examples");
    let mapped: usize = examples
        .iter()
        .flat_map(|e| &e.labels)
        .filter(|l| l.category.is_some())
        .count();
    assert!(
        mapped > 1800,
        "expected >1800 mapped gold spans, got {mapped}"
    );
}

#[test]
fn regex_prepass_meets_quality_floor() {
    let examples = corpus();
    let report = evaluate(&examples, regex_scan);

    let idx = |c: Category| {
        id4pii_core::eval::CATEGORIES
            .iter()
            .position(|&x| x == c)
            .unwrap()
    };
    let email = report.per_category[idx(Category::PrivateEmail)];
    let account = report.per_category[idx(Category::AccountNumber)];

    assert!(
        email.precision() >= 0.80,
        "email precision regressed: {:.3}",
        email.precision()
    );
    assert!(
        email.recall() >= 0.60,
        "email recall regressed: {:.3}",
        email.recall()
    );

    assert!(
        account.recall() >= 0.40,
        "account_number recall regressed: {:.3}",
        account.recall()
    );

    let targeted = [
        Category::PrivateEmail,
        Category::PrivateUrl,
        Category::PrivatePhone,
        Category::AccountNumber,
        Category::PrivateDate,
    ];
    let (mut good_preds, mut false_pos, mut matched, mut gold_total) = (0u32, 0u32, 0u32, 0u32);
    for c in targeted {
        let counts = report.per_category[idx(c)];
        good_preds += counts.good_preds;
        false_pos += counts.fp;
        matched += counts.matched;
        gold_total += counts.gold;
    }
    let precision = f64::from(good_preds) / f64::from(good_preds + false_pos).max(1.0);
    let recall = f64::from(matched) / f64::from(gold_total).max(1.0);
    assert!(
        precision >= 0.55,
        "targeted-category precision regressed: {precision:.3}"
    );
    assert!(
        recall >= 0.35,
        "targeted-category recall regressed: {recall:.3}"
    );
}
