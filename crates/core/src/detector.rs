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
        let mut builder =
            Session::builder()?.with_optimization_level(GraphOptimizationLevel::Level3)?;
        if threads > 0 {
            builder = builder.with_intra_threads(threads)?;
        }
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

    pub fn detect(&mut self, text: &str) -> Result<Vec<PiiSpan>> {
        if text.is_empty() {
            return Ok(Vec::new());
        }

        let tokens = self.tokenizer.encode_ordinary(text);
        let token_count = tokens.len();
        if token_count == 0 {
            return Ok(Vec::new());
        }

        let ids: Vec<i64> = tokens.iter().map(|&v| i64::from(v)).collect();
        let mask: Vec<i64> = vec![1; token_count];

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

        let run_start = std::time::Instant::now();
        let outputs = self.session.run(inputs![
            "input_ids" => TensorRef::from_array_view(([1_usize, token_count], ids.as_slice()))?,
            "attention_mask" => TensorRef::from_array_view(([1_usize, token_count], mask.as_slice()))?,
        ])?;
        tracing::debug!(elapsed = ?run_start.elapsed(), tokens = token_count, "inference complete");

        let (_, logits) = outputs[self.output_name.as_str()].try_extract_tensor::<f32>()?;
        let label_count = self.labels.len();
        if logits.len() != token_count * label_count {
            return Err(Error::Model(format!(
                "unexpected logits length {}, expected {}",
                logits.len(),
                token_count * label_count
            )));
        }

        let mut spans: Vec<PiiSpan> = Vec::new();
        let mut current: Option<SpanBuilder> = None;

        for token_index in 0..token_count {
            let row = &logits[token_index * label_count..(token_index + 1) * label_count];
            let (best, prob) = argmax_softmax(row);
            let category = self.labels.get(best).copied().flatten();
            let start = offsets[token_index];
            let end = offsets[token_index + 1];

            match category {
                Some(category) if start != end => match current.as_mut() {
                    Some(builder) if builder.category == category => {
                        builder.end = end;
                        builder.prob_sum += prob;
                        builder.token_count += 1;
                    }
                    _ => {
                        flush(&mut current, &mut spans, text);
                        current = Some(SpanBuilder {
                            category,
                            start,
                            end,
                            prob_sum: prob,
                            token_count: 1,
                        });
                    }
                },
                _ => flush(&mut current, &mut spans, text),
            }
        }
        flush(&mut current, &mut spans, text);
        Ok(spans)
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
}
