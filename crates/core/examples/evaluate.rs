//! Formal evaluation of the id4pii engine against the committed labelled corpus
//! (`crates/core/data/pii_dataset.tsv`).
//!
//! Always reports (model-free):
//!   * regex pre-pass precision / recall / F1 per category,
//!   * the token reduction the regex masking buys the model.
//!
//! When the model is present (`%LOCALAPPDATA%/id4pii/model/…`, populated by any `id4pii scan`),
//! it additionally compares **model-only** vs **hybrid** detection on both correctness and
//! wall-clock speed over the whole corpus — the formal version of the ad-hoc A/B.
//!
//! Run: `cargo run --release --example evaluate -p id4pii-core`
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]

use std::path::PathBuf;
use std::time::Instant;

use id4pii_core::eval::{Example, Report, evaluate, load_tsv};
use id4pii_core::{Detector, PiiSpan, model_dir, regex_scan};

fn dataset_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/pii_dataset.tsv")
}

/// Replicate the engine's masking (each regex hit → single space) for the token-reduction metric.
fn mask_text(text: &str, spans: &[PiiSpan]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for s in spans {
        if s.start < cursor || s.end > text.len() {
            continue;
        }
        out.push_str(&text[cursor..s.start]);
        out.push(' ');
        cursor = s.end;
    }
    out.push_str(&text[cursor..]);
    out
}

fn corpus_bytes(examples: &[Example]) -> usize {
    examples.iter().map(|e| e.text.len()).sum()
}

/// Run `detect` over the corpus, returning the report and the elapsed detection time.
fn timed_eval<F>(examples: &[Example], detect: F) -> (Report, std::time::Duration)
where
    F: FnMut(&str) -> Vec<PiiSpan>,
{
    let start = Instant::now();
    let report = evaluate(examples, detect);
    (report, start.elapsed())
}

fn throughput_line(label: &str, examples: &[Example], elapsed: std::time::Duration) {
    let secs = elapsed.as_secs_f64();
    let mb = corpus_bytes(examples) as f64 / 1_000_000.0;
    println!(
        "{label}: {:.3}s over {} examples ({:.0} ex/s, {:.2} MB/s)",
        secs,
        examples.len(),
        examples.len() as f64 / secs,
        mb / secs,
    );
}

fn main() {
    let path = dataset_path();
    let examples = match load_tsv(&path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!(
                "failed to load {}: {e}\nrun `python scripts/fetch-pii-dataset.py`",
                path.display()
            );
            std::process::exit(1);
        }
    };
    println!(
        "Corpus: {} examples, {} bytes ({})\n",
        examples.len(),
        corpus_bytes(&examples),
        path.display()
    );

    // ---- 1. Regex pre-pass (model-free) ----
    println!("== Regex pre-pass (model-free) ==");
    let (regex_report, regex_time) = timed_eval(&examples, regex_scan);
    print!("{}", regex_report.format_table());
    throughput_line("regex_scan", &examples, regex_time);
    println!("(person/address have no regex coverage by design — the model supplies those)\n");

    // ---- 2. Token reduction the masking buys the model (model-free) ----
    let bpe = tiktoken_rs::o200k_base().expect("tokenizer");
    let mut orig = 0usize;
    let mut masked = 0usize;
    for ex in &examples {
        let spans = regex_scan(&ex.text);
        orig += bpe.encode_ordinary(&ex.text).len();
        masked += bpe.encode_ordinary(&mask_text(&ex.text, &spans)).len();
    }
    println!("== Token load on the model ==");
    println!(
        "tokens fed to model: {orig} (full) -> {masked} (regex-masked) = {:.1}% reduction\n",
        if orig == 0 {
            0.0
        } else {
            (orig - masked) as f64 / orig as f64 * 100.0
        }
    );

    // ---- 3. Model-only vs hybrid (only when the model is present) ----
    let dir = model_dir::default_dir();
    if !model_dir::is_complete(&dir, model_dir::DEFAULT_MODEL_FILE) {
        println!(
            "Model not found at {} — skipping model-only/hybrid evaluation.",
            dir.display()
        );
        println!("Populate it by running `id4pii scan \"hello\"` once, then re-run this example.");
        return;
    }

    let mut detector = match Detector::load(&dir, model_dir::DEFAULT_MODEL_FILE, 0) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("failed to load model: {e}");
            return;
        }
    };

    // The model passes run two inferences per example, so by default they evaluate a sample of
    // the corpus to stay quick. Set ID4PII_EVAL_LIMIT=0 for the full corpus.
    let limit = std::env::var("ID4PII_EVAL_LIMIT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(400);
    let sample: &[Example] = if limit == 0 || limit >= examples.len() {
        &examples
    } else {
        &examples[..limit]
    };
    println!(
        "(model passes use a {}-example sample; set ID4PII_EVAL_LIMIT=0 for all {})\n",
        sample.len(),
        examples.len()
    );

    println!("== Model-only (regex pre-pass disabled) ==");
    detector.set_regex_enabled(false);
    let (model_report, model_time) =
        timed_eval(sample, |t| detector.detect(t, 0.0).unwrap_or_default());
    print!("{}", model_report.format_table());
    throughput_line("model-only", sample, model_time);
    println!();

    println!("== Hybrid (regex pre-pass + model) ==");
    detector.set_regex_enabled(true);
    let (hybrid_report, hybrid_time) =
        timed_eval(sample, |t| detector.detect(t, 0.0).unwrap_or_default());
    print!("{}", hybrid_report.format_table());
    throughput_line("hybrid", sample, hybrid_time);
    println!();

    // ---- Summary ----
    let mo = model_report.overall();
    let hy = hybrid_report.overall();
    let speedup = model_time.as_secs_f64() / hybrid_time.as_secs_f64().max(1e-9);
    println!("== Summary ==");
    println!(
        "overall F1:   model-only {:.1}%  ->  hybrid {:.1}%",
        mo.f1() * 100.0,
        hy.f1() * 100.0
    );
    println!(
        "detect time:  model-only {:.2}s  ->  hybrid {:.2}s  ({speedup:.2}x faster)",
        model_time.as_secs_f64(),
        hybrid_time.as_secs_f64()
    );
}
