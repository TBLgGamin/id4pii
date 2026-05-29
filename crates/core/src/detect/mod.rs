//! PII detection.
//!
//! Detection is a **hybrid pipeline** built from two recognizers that play to opposite
//! strengths:
//!
//! 1. [`regex`] — a single compiled DFA that catches structurally-regular PII and secrets
//!    (emails, URLs, phones, card/account numbers, dates, API keys) in one linear pass. Cheap
//!    and exact.
//! 2. [`model`] — the ONNX transformer, which catches the context-dependent categories regex
//!    cannot (people, addresses) and anything the patterns miss. Accurate but expensive.
//!
//! [`Detector::detect`] runs the regex first, [`mask`]s its hits out of the text, and only then
//! runs the model — on the *shortened* text. The model is the bottleneck, and its attention
//! cost grows super-linearly with token count, so removing the regex-found spans up front both
//! skips redundant work and shrinks the sequence the transformer has to chew through. The two
//! result sets are then merged back into one list in original-document coordinates, with regex
//! hits taking precedence on overlap.

mod mask;
mod model;
mod regex;

use std::path::Path;

use serde::Serialize;

use crate::error::Result;
use crate::labels::Category;
use model::ModelDetector;
use regex::RegexDetector;

/// A detected span of PII: its category, byte range within the source text, the matched text,
/// and an averaged confidence in `0.0..=1.0` (regex matches are reported as `1.0`).
#[derive(Debug, Clone, Serialize)]
pub struct PiiSpan {
    pub category: Category,
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub score: f32,
}

/// Environment variable that, when set to `0`/`false`, disables the regex pre-pass and runs the
/// model over the full text. Used for A/B latency comparison; on by default.
const REGEX_ENV: &str = "ID4PII_REGEX";

/// The hybrid detector: a regex pre-filter feeding a shortened text to the ONNX model.
pub struct Detector {
    model: ModelDetector,
    regex: &'static RegexDetector,
    use_regex: bool,
}

impl std::fmt::Debug for Detector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Detector")
            .field("model", &self.model)
            .field("use_regex", &self.use_regex)
            .finish_non_exhaustive()
    }
}

impl Detector {
    /// Load the model and wire up the shared regex pre-filter. See [`ModelDetector::load`] for
    /// the `threads` semantics.
    pub fn load(model_dir: &Path, model_file: &str, threads: usize) -> Result<Self> {
        let model = ModelDetector::load(model_dir, model_file, threads)?;
        let use_regex = std::env::var(REGEX_ENV)
            .map_or(true, |v| !(v == "0" || v.eq_ignore_ascii_case("false")));
        Ok(Self {
            model,
            regex: RegexDetector::global(),
            use_regex,
        })
    }

    /// Whether the regex pre-pass is active.
    #[must_use]
    pub fn regex_enabled(&self) -> bool {
        self.use_regex
    }

    /// Enable or disable the regex pre-pass at runtime (used by benchmarks/tools to A/B the
    /// model-only path against the hybrid path).
    pub fn set_regex_enabled(&mut self, enabled: bool) {
        self.use_regex = enabled;
    }

    /// Detect PII spans in `text`, dropping model spans scoring below `min_score` (pass `0.0`
    /// to keep everything). Regex hits are always kept (confidence `1.0`).
    pub fn detect(&mut self, text: &str, min_score: f32) -> Result<Vec<PiiSpan>> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        if !self.use_regex {
            return self.model.detect(text, min_score);
        }

        let regex_spans = self.regex.detect(text);
        if regex_spans.is_empty() {
            return self.model.detect(text, min_score);
        }

        let masked = mask::mask(text, &regex_spans);
        let model_spans = self.model.detect(&masked.text, min_score)?;
        Ok(combine(text, regex_spans, &model_spans, &masked))
    }

    /// Run the model over the full, unmasked text — the pre-hybrid behaviour. Exposed for
    /// latency comparison.
    pub fn detect_model_only(&mut self, text: &str, min_score: f32) -> Result<Vec<PiiSpan>> {
        self.model.detect(text, min_score)
    }

    /// Detect PII across many texts at once. The cheap per-text regex pre-pass and masking run
    /// individually, but every masked text's model inference is batched into shared `run` calls,
    /// so a burst of requests pays the large fixed per-`run` cost collectively. The result is
    /// aligned with `texts`.
    pub fn detect_batch(&mut self, texts: &[&str], min_score: f32) -> Result<Vec<Vec<PiiSpan>>> {
        if !self.use_regex {
            return self.model.detect_batch(texts, min_score);
        }
        let regex_spans: Vec<Vec<PiiSpan>> = texts.iter().map(|t| self.regex.detect(t)).collect();
        let masked: Vec<mask::Masked> = texts
            .iter()
            .zip(&regex_spans)
            .map(|(text, spans)| mask::mask(text, spans))
            .collect();
        let masked_refs: Vec<&str> = masked.iter().map(|m| m.text.as_str()).collect();
        let model_spans = self.model.detect_batch(&masked_refs, min_score)?;

        let mut out = Vec::with_capacity(texts.len());
        for (index, regex) in regex_spans.into_iter().enumerate() {
            out.push(combine(texts[index], regex, &model_spans[index], &masked[index]));
        }
        Ok(out)
    }
}

/// Merge the regex spans (already in original coordinates) with the model spans (in masked
/// coordinates), translating the latter back, dropping any model span that overlaps a regex hit
/// (regex wins), then unioning same-category overlaps.
fn combine(
    text: &str,
    regex_spans: Vec<PiiSpan>,
    model_spans: &[PiiSpan],
    masked: &mask::Masked,
) -> Vec<PiiSpan> {
    let mut out = regex_spans;
    let regex_count = out.len();
    for span in model_spans {
        let start = masked.map_start(span.start);
        let end = masked.map_end(span.end);
        if start >= end || end > text.len() {
            continue;
        }
        // Drop any model span overlapping a regex hit (regex wins). The borrow of the regex
        // prefix ends before the `push` below, so this does not conflict.
        let overlaps_regex = out[..regex_count]
            .iter()
            .any(|r| r.start < end && start < r.end);
        if overlaps_regex {
            continue;
        }
        let Some(slice) = text.get(start..end) else {
            continue;
        };
        out.push(PiiSpan {
            category: span.category,
            start,
            end,
            text: slice.to_string(),
            score: span.score,
        });
    }
    out.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    merge_overlapping(&mut out, text);
    out
}

/// Merge spans collected across overlapping windows (or from both detectors). Same-category
/// spans whose byte ranges strictly overlap are unioned (keeping the longer span's score);
/// disjoint spans are left untouched, so single-window output is unaffected. Input must be
/// sorted by start (longer span first on ties) so a boundary fragment merges into the full span
/// that subsumes it.
fn merge_overlapping(spans: &mut Vec<PiiSpan>, text: &str) {
    if spans.len() < 2 {
        return;
    }
    spans.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut merged: Vec<PiiSpan> = Vec::with_capacity(spans.len());
    for span in spans.drain(..) {
        if let Some(last) = merged.last_mut()
            && span.category == last.category
            && span.start < last.end
        {
            if span.end - span.start > last.end - last.start {
                last.score = span.score;
            }
            if span.end > last.end {
                last.end = span.end;
                if let Some(slice) = text.get(last.start..last.end) {
                    last.text = slice.to_string();
                }
            }
            continue;
        }
        merged.push(span);
    }
    *spans = merged;
}

/// Scan `text` with only the fast regex pre-filter — no model required. Useful for callers that
/// want the cheap structural matches alone, and for benchmarking the pre-pass in isolation.
#[must_use]
pub fn regex_scan(text: &str) -> Vec<PiiSpan> {
    RegexDetector::global().detect(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(category: Category, start: usize, end: usize, text: &str, score: f32) -> PiiSpan {
        PiiSpan {
            category,
            start,
            end,
            text: text.to_string(),
            score,
        }
    }

    #[test]
    fn merge_collapses_boundary_fragment_into_full_span() {
        let text = "John Smith";
        let mut spans = vec![
            span(Category::PrivatePerson, 0, 4, "John", 0.5),
            span(Category::PrivatePerson, 0, 10, "John Smith", 0.9),
        ];
        merge_overlapping(&mut spans, text);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].end, 10);
        assert_eq!(spans[0].text, "John Smith");
        assert!((spans[0].score - 0.9).abs() < 1e-6);
    }

    #[test]
    fn merge_keeps_disjoint_same_category_spans_separate() {
        let text = "John and Mary";
        let mut spans = vec![
            span(Category::PrivatePerson, 0, 4, "John", 1.0),
            span(Category::PrivatePerson, 9, 13, "Mary", 1.0),
        ];
        merge_overlapping(&mut spans, text);
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn merge_does_not_join_across_categories() {
        let text = "0123456789";
        let mut spans = vec![
            span(Category::AccountNumber, 0, 6, "012345", 1.0),
            span(Category::PrivatePhone, 4, 10, "456789", 1.0),
        ];
        merge_overlapping(&mut spans, text);
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn merge_dedups_identical_spans() {
        let text = "John Smith";
        let mut spans = vec![
            span(Category::PrivatePerson, 0, 10, "John Smith", 0.8),
            span(Category::PrivatePerson, 0, 10, "John Smith", 0.8),
        ];
        merge_overlapping(&mut spans, text);
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn combine_drops_model_span_overlapping_a_regex_hit() {
        // Regex found the email; a stray model span over the same bytes must be discarded.
        let text = "ping a@b.com please";
        let regex_spans = vec![span(Category::PrivateEmail, 5, 12, "a@b.com", 1.0)];
        let masked = mask::mask(text, &regex_spans);
        // Fabricate a model span (masked coords) that maps back onto the email region.
        let model = vec![span(Category::PrivatePerson, 5, 6, " ", 0.9)];
        let combined = combine(text, regex_spans, &model, &masked);
        assert_eq!(combined.len(), 1);
        assert_eq!(combined[0].category, Category::PrivateEmail);
    }
}
