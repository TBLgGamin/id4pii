use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use ort::inputs;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::TensorRef;
use serde::Deserialize;
use tiktoken_rs::CoreBPE;

use crate::error::{Error, Result};
use crate::labels::{Category, load_label_map};

/// Token-window size for long inputs. Sequences at or below this length are detected in a
/// single inference pass (identical to processing the whole input at once); longer ones are
/// split into windows of this size. Sized so a typical chat field stays single-pass.
const DETECT_WINDOW: usize = 1024;
/// Token overlap between adjacent windows. Must exceed the longest expected entity so any
/// entity straddling a window boundary is fully contained in at least one window.
const DETECT_OVERLAP: usize = 128;

/// Intra-op thread count used when the caller does not specify one (`threads == 0`).
/// Deliberately small: ONNX Runtime's intra-op pool deadlocks under many sequential `run`
/// calls (which windowed detection makes) once the thread count is high, while 1–2 threads
/// run reliably; the model is small enough that more threads add little. Do not raise this to
/// the core count "for speed" — that reintroduces the hang.
const DEFAULT_INTRA_THREADS: usize = 2;

#[derive(Debug, Clone, serde::Serialize)]
pub struct PiiSpan {
    pub category: Category,
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub score: f32,
}

#[derive(Deserialize)]
struct ModelConfig {
    id2label: BTreeMap<String, String>,
}

pub struct Detector {
    session: Session,
    tokenizer: CoreBPE,
    labels: Vec<Option<Category>>,
    output_name: String,
}

impl fmt::Debug for Detector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Detector")
            .field("label_count", &self.labels.len())
            .field("output_name", &self.output_name)
            .finish_non_exhaustive()
    }
}

impl Detector {
    pub fn load(model_dir: &Path, model_file: &str, threads: usize) -> Result<Self> {
        let tokenizer_handle = std::thread::spawn(|| -> Result<CoreBPE> {
            let started = std::time::Instant::now();
            let tokenizer =
                tiktoken_rs::o200k_base().map_err(|e| Error::Tokenizer(e.to_string()))?;
            tracing::debug!(elapsed = ?started.elapsed(), "tokenizer ready");
            Ok(tokenizer)
        });

        let config_text = std::fs::read_to_string(model_dir.join("config.json"))?;
        let config: ModelConfig = serde_json::from_str(&config_text)?;
        let labels = load_label_map(&config.id2label)?;

        let session_start = std::time::Instant::now();
        let intra_threads = if threads > 0 {
            threads
        } else {
            DEFAULT_INTRA_THREADS
        };
        let mut builder = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_intra_op_spinning(false)?
            .with_intra_threads(intra_threads)?;
        let session = builder.commit_from_file(model_dir.join(model_file))?;
        tracing::debug!(elapsed = ?session_start.elapsed(), "onnx session ready");

        let tokenizer = tokenizer_handle
            .join()
            .map_err(|_| Error::Model("tokenizer load thread panicked".into()))??;

        let output_name = session
            .outputs()
            .first()
            .map(|output| output.name().to_string())
            .ok_or_else(|| Error::Model("model exposes no outputs".into()))?;

        Ok(Self {
            session,
            tokenizer,
            labels,
            output_name,
        })
    }

    /// Detect PII spans in `text`. Spans scoring below `min_score` are dropped (pass `0.0`
    /// to keep every detection). Inputs longer than [`DETECT_WINDOW`] tokens are processed in
    /// overlapping windows and the per-window spans are merged, so detection cost stays linear
    /// in length instead of quadratic; inputs within one window take a single inference pass.
    pub fn detect(&mut self, text: &str, min_score: f32) -> Result<Vec<PiiSpan>> {
        if text.is_empty() {
            return Ok(Vec::new());
        }

        let tokens = self.tokenizer.encode_ordinary(text);
        let token_count = tokens.len();
        if token_count == 0 {
            return Ok(Vec::new());
        }

        let mut offsets: Vec<usize> = Vec::with_capacity(token_count + 1);
        offsets.push(0);
        let mut cursor = 0_usize;
        for &token in &tokens {
            cursor += self
                .tokenizer
                .decode_bytes(&[token])
                .map_err(|e| Error::Tokenizer(e.to_string()))?
                .len();
            offsets.push(cursor);
        }

        let mut spans: Vec<PiiSpan> = Vec::new();
        if token_count <= DETECT_WINDOW {
            self.detect_window(text, &tokens, &offsets, 0, &mut spans)?;
        } else {
            let step = DETECT_WINDOW - DETECT_OVERLAP;
            let mut start = 0;
            loop {
                let end = (start + DETECT_WINDOW).min(token_count);
                self.detect_window(text, &tokens[start..end], &offsets, start, &mut spans)?;
                if end == token_count {
                    break;
                }
                start += step;
            }
            merge_overlapping(&mut spans, text);
        }

        if min_score > 0.0 {
            spans.retain(|span| span.score >= min_score);
        }
        Ok(spans)
    }

    /// Run inference on a single token window `tokens` (a slice of the full token sequence
    /// starting at `token_start`) and append the decoded spans to `spans`. Byte offsets are
    /// resolved through the full-text `offsets` table so spans carry document-absolute ranges.
    fn detect_window(
        &mut self,
        text: &str,
        tokens: &[u32],
        offsets: &[usize],
        token_start: usize,
        spans: &mut Vec<PiiSpan>,
    ) -> Result<()> {
        let window_len = tokens.len();
        if window_len == 0 {
            return Ok(());
        }
        let ids: Vec<i64> = tokens.iter().map(|&v| i64::from(v)).collect();
        let mask: Vec<i64> = vec![1; window_len];

        let run_start = std::time::Instant::now();
        let outputs = self.session.run(inputs![
            "input_ids" => TensorRef::from_array_view(([1_usize, window_len], ids.as_slice()))?,
            "attention_mask" => TensorRef::from_array_view(([1_usize, window_len], mask.as_slice()))?,
        ])?;
        tracing::debug!(elapsed = ?run_start.elapsed(), tokens = window_len, "inference complete");

        let (_, logits) = outputs[self.output_name.as_str()].try_extract_tensor::<f32>()?;
        let label_count = self.labels.len();
        if logits.len() != window_len * label_count {
            return Err(Error::Model(format!(
                "unexpected logits length {}, expected {}",
                logits.len(),
                window_len * label_count
            )));
        }

        let mut current: Option<SpanBuilder> = None;
        for token_index in 0..window_len {
            let row = &logits[token_index * label_count..(token_index + 1) * label_count];
            let (best, prob) = argmax_softmax(row);
            let category = self.labels.get(best).copied().flatten();
            let start = offsets[token_start + token_index];
            let end = offsets[token_start + token_index + 1];

            match category {
                Some(category) if start != end => match current.as_mut() {
                    Some(builder) if builder.category == category => {
                        builder.end = end;
                        builder.prob_sum += prob;
                        builder.token_count += 1;
                    }
                    _ => {
                        flush(&mut current, spans, text);
                        current = Some(SpanBuilder {
                            category,
                            start,
                            end,
                            prob_sum: prob,
                            token_count: 1,
                        });
                    }
                },
                _ => flush(&mut current, spans, text),
            }
        }
        flush(&mut current, spans, text);
        Ok(())
    }
}

struct SpanBuilder {
    category: Category,
    start: usize,
    end: usize,
    prob_sum: f32,
    token_count: usize,
}

fn flush(current: &mut Option<SpanBuilder>, spans: &mut Vec<PiiSpan>, text: &str) {
    let Some(builder) = current.take() else {
        return;
    };
    let Some(slice) = text.get(builder.start..builder.end) else {
        return;
    };
    let start = builder.start + (slice.len() - slice.trim_start().len());
    let end = builder.end - (slice.len() - slice.trim_end().len());
    if start >= end {
        return;
    }
    let Some(trimmed) = text.get(start..end) else {
        return;
    };
    spans.push(PiiSpan {
        category: builder.category,
        start,
        end,
        text: trimmed.to_string(),
        score: builder.prob_sum / builder.token_count.max(1) as f32,
    });
}

/// Merge spans collected across overlapping windows. Same-category spans whose byte ranges
/// strictly overlap are unioned (keeping the longer span's score); disjoint spans are left
/// untouched, so single-window output is unaffected. Spans must be re-sorted afterwards by
/// the caller's contract — here we sort by start (longer span first on ties) so a boundary
/// fragment always merges into the full span that subsumes it.
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

fn argmax_softmax(row: &[f32]) -> (usize, f32) {
    let max = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut sum = 0.0_f32;
    let mut best_index = 0;
    let mut best_logit = f32::NEG_INFINITY;
    for (index, &value) in row.iter().enumerate() {
        sum += (value - max).exp();
        if value > best_logit {
            best_logit = value;
            best_index = index;
        }
    }
    let prob = if sum > 0.0 { 1.0 / sum } else { 0.0 };
    (best_index, prob)
}

#[cfg(test)]
#[allow(clippy::unreadable_literal)]
mod tests {
    #[test]
    fn tokenizer_matches_privacy_filter_reference() {
        let bpe = tiktoken_rs::o200k_base().unwrap();
        let ids = bpe.encode_ordinary("Email alice@acme.com or call 555-0142 about account 11829");
        assert_eq!(
            ids,
            vec![
                6622, 134271, 31, 359, 1047, 1136, 503, 2421, 220, 22275, 12, 28207, 17, 1078,
                3527, 220, 14642, 2270
            ]
        );
    }

    #[test]
    fn token_byte_offsets_sum_to_text_length() {
        let bpe = tiktoken_rs::o200k_base().unwrap();
        let text = "naïve café 🚀 reach me at test@x.com";
        let tokens = bpe.encode_ordinary(text);
        let total: usize = tokens
            .iter()
            .map(|&t| bpe.decode_bytes(&[t]).unwrap().len())
            .sum();
        assert_eq!(total, text.len());
    }

    use super::{Category, PiiSpan, merge_overlapping};

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
}
