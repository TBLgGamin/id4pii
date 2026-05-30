mod mask;
mod model;
mod regex;

use std::path::Path;

use serde::Serialize;

use crate::error::Result;
use crate::labels::Category;
use model::ModelDetector;
use regex::RegexDetector;

#[derive(Debug, Clone, Serialize)]
pub struct PiiSpan {
    pub category: Category,
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub score: f32,
}

const REGEX_ENV: &str = "ID4PII_REGEX";

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

    #[must_use]
    pub fn regex_enabled(&self) -> bool {
        self.use_regex
    }

    pub fn set_regex_enabled(&mut self, enabled: bool) {
        self.use_regex = enabled;
    }

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

    pub fn detect_model_only(&mut self, text: &str, min_score: f32) -> Result<Vec<PiiSpan>> {
        self.model.detect(text, min_score)
    }

    pub fn detect_batch(&mut self, texts: &[&str], min_score: f32) -> Result<Vec<Vec<PiiSpan>>> {
        self.detect_many(texts, min_score, None)
    }

    pub fn detect_corpus(
        &mut self,
        texts: &[&str],
        min_score: f32,
        batch_size: usize,
    ) -> Result<Vec<Vec<PiiSpan>>> {
        self.detect_many(texts, min_score, Some(batch_size))
    }

    fn detect_many(
        &mut self,
        texts: &[&str],
        min_score: f32,
        batch_size: Option<usize>,
    ) -> Result<Vec<Vec<PiiSpan>>> {
        let run_model = |model: &mut ModelDetector, inputs: &[&str]| match batch_size {
            Some(size) => model.detect_corpus(inputs, min_score, size),
            None => model.detect_batch(inputs, min_score),
        };

        if !self.use_regex {
            return run_model(&mut self.model, texts);
        }
        let regex_spans: Vec<Vec<PiiSpan>> = texts.iter().map(|t| self.regex.detect(t)).collect();
        let masked: Vec<mask::Masked> = texts
            .iter()
            .zip(&regex_spans)
            .map(|(text, spans)| mask::mask(text, spans))
            .collect();
        let masked_refs: Vec<&str> = masked.iter().map(|m| m.text.as_str()).collect();
        let model_spans = run_model(&mut self.model, &masked_refs)?;

        let mut out = Vec::with_capacity(texts.len());
        for (index, regex) in regex_spans.into_iter().enumerate() {
            out.push(combine(
                texts[index],
                regex,
                &model_spans[index],
                &masked[index],
            ));
        }
        Ok(out)
    }
}

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
        let text = "ping a@b.com please";
        let regex_spans = vec![span(Category::PrivateEmail, 5, 12, "a@b.com", 1.0)];
        let masked = mask::mask(text, &regex_spans);

        let model = vec![span(Category::PrivatePerson, 5, 6, " ", 0.9)];
        let combined = combine(text, regex_spans, &model, &masked);
        assert_eq!(combined.len(), 1);
        assert_eq!(combined[0].category, Category::PrivateEmail);
    }
}
