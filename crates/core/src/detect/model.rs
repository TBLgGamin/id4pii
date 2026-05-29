use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use ort::execution_providers::{CPUExecutionProvider, ExecutionProviderDispatch};
use ort::inputs;
use ort::session::Session;
use ort::session::builder::{GraphOptimizationLevel, SessionBuilder};
use ort::value::TensorRef;
use serde::Deserialize;
use tiktoken_rs::CoreBPE;

use super::PiiSpan;
use crate::error::{Error, Result};
use crate::labels::{Category, load_label_map};

const DETECT_WINDOW: usize = 1024;

const DETECT_OVERLAP: usize = 128;

const MAX_BATCH: usize = 4;

const DEFAULT_INTRA_THREADS: usize = 2;

const FORCE_CPU_ENV: &str = "ID4PII_CPU";

#[derive(Deserialize)]
struct ModelConfig {
    id2label: BTreeMap<String, String>,
}

pub(crate) struct ModelDetector {
    session: Session,
    tokenizer: CoreBPE,
    labels: Vec<Option<Category>>,
    output_name: String,

    token_len: HashMap<u32, usize>,
}

impl fmt::Debug for ModelDetector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelDetector")
            .field("label_count", &self.labels.len())
            .field("output_name", &self.output_name)
            .finish_non_exhaustive()
    }
}

fn execution_providers() -> Vec<ExecutionProviderDispatch> {
    let force_cpu =
        std::env::var(FORCE_CPU_ENV).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    let mut providers: Vec<ExecutionProviderDispatch> = Vec::new();
    if !force_cpu {
        #[cfg(feature = "directml")]
        providers.push(ort::execution_providers::DirectMLExecutionProvider::default().build());
        #[cfg(feature = "cuda")]
        providers.push(ort::execution_providers::CUDAExecutionProvider::default().build());
    }
    providers.push(
        CPUExecutionProvider::default()
            .with_arena_allocator(true)
            .build(),
    );
    providers
}

impl ModelDetector {
    pub(crate) fn load(model_dir: &Path, model_file: &str, threads: usize) -> Result<Self> {
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
        let mut builder: SessionBuilder = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_memory_pattern(true)?
            .with_intra_op_spinning(false)?
            .with_intra_threads(intra_threads)?
            .with_execution_providers(execution_providers())?;
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
            token_len: HashMap::new(),
        })
    }

    fn token_byte_len(&mut self, token: u32) -> Result<usize> {
        if let Some(&len) = self.token_len.get(&token) {
            return Ok(len);
        }
        let len = self
            .tokenizer
            .decode_bytes(&[token])
            .map_err(|e| Error::Tokenizer(e.to_string()))?
            .len();
        self.token_len.insert(token, len);
        Ok(len)
    }

    fn tokenize(&mut self, text: &str) -> Result<(Vec<u32>, Vec<usize>)> {
        let tokens = self.tokenizer.encode_ordinary(text);
        let mut offsets = Vec::with_capacity(tokens.len() + 1);
        offsets.push(0);
        let mut cursor = 0usize;
        for &token in &tokens {
            cursor += self.token_byte_len(token)?;
            offsets.push(cursor);
        }
        Ok((tokens, offsets))
    }

    pub(crate) fn detect(&mut self, text: &str, min_score: f32) -> Result<Vec<PiiSpan>> {
        let results = self.detect_batch(&[text], min_score)?;
        Ok(results.into_iter().next().unwrap_or_default())
    }

    pub(crate) fn detect_batch(
        &mut self,
        texts: &[&str],
        min_score: f32,
    ) -> Result<Vec<Vec<PiiSpan>>> {
        let mut tokens: Vec<Vec<u32>> = Vec::with_capacity(texts.len());
        let mut offsets: Vec<Vec<usize>> = Vec::with_capacity(texts.len());
        for text in texts {
            if text.is_empty() {
                tokens.push(Vec::new());
                offsets.push(vec![0]);
                continue;
            }
            let (tok, off) = self.tokenize(text)?;
            tokens.push(tok);
            offsets.push(off);
        }

        let mut windows: Vec<Window> = Vec::new();
        let mut multi_window = vec![false; texts.len()];
        for (index, text) in texts.iter().enumerate() {
            let tok = &tokens[index];
            let off = &offsets[index];
            if tok.is_empty() {
                continue;
            }
            if tok.len() <= DETECT_WINDOW {
                windows.push(Window {
                    tokens: tok,
                    offsets: off,
                    token_start: 0,
                    text,
                    out_index: index,
                });
            } else {
                multi_window[index] = true;
                let step = DETECT_WINDOW - DETECT_OVERLAP;
                let mut start = 0;
                loop {
                    let end = (start + DETECT_WINDOW).min(tok.len());
                    windows.push(Window {
                        tokens: &tok[start..end],
                        offsets: off,
                        token_start: start,
                        text,
                        out_index: index,
                    });
                    if end == tok.len() {
                        break;
                    }
                    start += step;
                }
            }
        }

        let mut out = vec![Vec::new(); texts.len()];
        self.run_and_decode(&windows, &mut out)?;

        for (index, spans) in out.iter_mut().enumerate() {
            if multi_window[index] {
                super::merge_overlapping(spans, texts[index]);
            }
            if min_score > 0.0 {
                spans.retain(|span| span.score >= min_score);
            }
        }
        Ok(out)
    }

    fn run_and_decode(&mut self, windows: &[Window], out: &mut [Vec<PiiSpan>]) -> Result<()> {
        let label_count = self.labels.len();
        for chunk in windows.chunks(MAX_BATCH) {
            let batch = chunk.len();
            let padded = chunk.iter().map(|w| w.tokens.len()).max().unwrap_or(0);
            if padded == 0 {
                continue;
            }
            let mut ids = vec![0i64; batch * padded];
            let mut mask = vec![0i64; batch * padded];
            for (b, window) in chunk.iter().enumerate() {
                let base = b * padded;
                for (i, &token) in window.tokens.iter().enumerate() {
                    ids[base + i] = i64::from(token);
                    mask[base + i] = 1;
                }
            }

            let run_start = std::time::Instant::now();
            let outputs = self.session.run(inputs![
                "input_ids" => TensorRef::from_array_view(([batch, padded], ids.as_slice()))?,
                "attention_mask" => TensorRef::from_array_view(([batch, padded], mask.as_slice()))?,
            ])?;
            tracing::debug!(
                elapsed = ?run_start.elapsed(),
                batch,
                tokens = padded,
                "inference complete"
            );

            let (_, logits) = outputs[self.output_name.as_str()].try_extract_tensor::<f32>()?;
            if logits.len() != batch * padded * label_count {
                return Err(Error::Model(format!(
                    "unexpected logits length {}, expected {}",
                    logits.len(),
                    batch * padded * label_count
                )));
            }
            for (b, window) in chunk.iter().enumerate() {
                let base = b * padded * label_count;
                let window_logits = &logits[base..base + padded * label_count];
                decode_window(window_logits, window, label_count, &self.labels, out);
            }
        }
        Ok(())
    }
}

struct Window<'a> {
    tokens: &'a [u32],
    offsets: &'a [usize],
    token_start: usize,
    text: &'a str,
    out_index: usize,
}

fn decode_window(
    logits: &[f32],
    window: &Window,
    label_count: usize,
    labels: &[Option<Category>],
    out: &mut [Vec<PiiSpan>],
) {
    let spans = &mut out[window.out_index];
    let mut current: Option<SpanBuilder> = None;
    for i in 0..window.tokens.len() {
        let row = &logits[i * label_count..(i + 1) * label_count];
        let (best, prob) = argmax_softmax(row);
        let category = labels.get(best).copied().flatten();
        let start = window.offsets[window.token_start + i];
        let end = window.offsets[window.token_start + i + 1];

        match category {
            Some(category) if start != end => match current.as_mut() {
                Some(builder) if builder.category == category => {
                    builder.end = end;
                    builder.prob_sum += prob;
                    builder.token_count += 1;
                }
                _ => {
                    flush(&mut current, spans, window.text);
                    current = Some(SpanBuilder {
                        category,
                        start,
                        end,
                        prob_sum: prob,
                        token_count: 1,
                    });
                }
            },
            _ => flush(&mut current, spans, window.text),
        }
    }
    flush(&mut current, spans, window.text);
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
