//! The transformer half of detection: ONNX Runtime inference over the
//! [`openai/privacy-filter`](https://huggingface.co/openai/privacy-filter) token-classifier,
//! plus span decoding. This is the slow, contextual recognizer. The hybrid orchestrator in
//! [`super`] runs the cheap [`regex`](super::regex) pre-pass first and feeds this model a
//! shortened text, so the heavy inference processes fewer tokens.

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

/// Token-window size for long inputs. Sequences at or below this length are detected in a
/// single inference pass (identical to processing the whole input at once); longer ones are
/// split into windows of this size. Sized so a typical chat field stays single-pass.
const DETECT_WINDOW: usize = 1024;
/// Token overlap between adjacent windows. Must exceed the longest expected entity so any
/// entity straddling a window boundary is fully contained in at least one window.
const DETECT_OVERLAP: usize = 128;

/// Maximum sequences fed to ONNX Runtime in a single batched `run`. Windowed inputs and batched
/// requests are split into chunks of this size: bigger chunks amortize the large fixed per-`run`
/// cost over more sequences, smaller chunks bound the peak attention memory. Each batch element
/// holds its own up-to-`DETECT_WINDOW`-token attention, so peak memory grows with this; 4 keeps
/// it to a few times a single window while still collapsing most inputs into one call.
const MAX_BATCH: usize = 4;

/// Intra-op thread count used when the caller does not specify one (`threads == 0`).
/// Deliberately small: ONNX Runtime's intra-op pool deadlocks under many sequential `run`
/// calls (which windowed detection makes) once the thread count is high, while 1–2 threads
/// run reliably; the model is small enough that more threads add little. Do not raise this to
/// the core count "for speed" — that reintroduces the hang.
const DEFAULT_INTRA_THREADS: usize = 2;

/// Environment variable that forces the CPU execution provider even in a build that bundles a
/// GPU provider (`--features cuda`/`directml`). Useful for A/B and for debugging.
const FORCE_CPU_ENV: &str = "ID4PII_CPU";

#[derive(Deserialize)]
struct ModelConfig {
    id2label: BTreeMap<String, String>,
}

/// ONNX token-classifier wrapped with the embedded tokenizer and label map. Owns a small
/// token-length cache so the byte-offset table — rebuilt on every `detect` call — does not
/// re-decode the same token ids over and over across the warm `serve`/guard request loop.
pub(crate) struct ModelDetector {
    session: Session,
    tokenizer: CoreBPE,
    labels: Vec<Option<Category>>,
    output_name: String,
    /// `token id -> decoded byte length`. Byte length is a pure function of the id, so this is
    /// always safe to memoize; it turns the per-token `decode_bytes` allocation into a hashmap
    /// hit once a token has been seen.
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

/// Build the ordered execution-provider list. GPU providers (registered only in a build with the
/// matching cargo feature) are tried first with non-fatal registration, so a machine without the
/// GPU/runtime silently falls back to the CPU provider, which always closes the list with its
/// arena allocator enabled (buffer reuse across runs).
fn execution_providers() -> Vec<ExecutionProviderDispatch> {
    let force_cpu = std::env::var(FORCE_CPU_ENV)
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    let mut providers: Vec<ExecutionProviderDispatch> = Vec::new();
    if !force_cpu {
        #[cfg(feature = "directml")]
        providers.push(ort::execution_providers::DirectMLExecutionProvider::default().build());
        #[cfg(feature = "cuda")]
        providers.push(ort::execution_providers::CUDAExecutionProvider::default().build());
    }
    providers.push(CPUExecutionProvider::default().with_arena_allocator(true).build());
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

    /// Decoded byte length of a single token, memoized. The first time a token id is seen it is
    /// decoded once; afterwards it is a hashmap lookup.
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

    /// Tokenize `text` and build its token byte-offset table (`offsets[i]` is the byte index
    /// where token `i` begins; `offsets[len]` is the text length).
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

    /// Detect PII spans in `text`. Spans scoring below `min_score` are dropped (pass `0.0`
    /// to keep every detection). Inputs longer than [`DETECT_WINDOW`] tokens are processed in
    /// overlapping windows; all windows are fed to ONNX Runtime as a single batched inference
    /// (in chunks of [`MAX_BATCH`]) so the large fixed per-`run` cost is paid once, not per
    /// window. Inputs within one window take a single sequence.
    pub(crate) fn detect(&mut self, text: &str, min_score: f32) -> Result<Vec<PiiSpan>> {
        let results = self.detect_batch(&[text], min_score)?;
        Ok(results.into_iter().next().unwrap_or_default())
    }

    /// Detect PII in many texts at once, batching every text's window(s) into shared inferences
    /// so a burst of requests pays the fixed per-`run` cost collectively rather than each. The
    /// returned vector is aligned with `texts`.
    pub(crate) fn detect_batch(
        &mut self,
        texts: &[&str],
        min_score: f32,
    ) -> Result<Vec<Vec<PiiSpan>>> {
        // Pass 1: tokenize every text up front so the per-text token/offset buffers are stable
        // before any `Window` borrows into them (growing the storage would invalidate refs).
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

        // Pass 2: build the window list referencing the now-stable storage.
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

    /// Run `windows` through the model in batched chunks and decode each window's logits into the
    /// span list at its `out_index`. Sequences in a chunk are right-padded to the longest one and
    /// padding is masked out via `attention_mask`, so a row's outputs depend only on its own
    /// tokens — batching is equivalent to running each window alone.
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

/// One token sequence to classify: a (possibly windowed) slice of a text's tokens, the full
/// text's offset table, the token index the slice starts at, the source text, and which output
/// span list the decoded spans belong to.
struct Window<'a> {
    tokens: &'a [u32],
    offsets: &'a [usize],
    token_start: usize,
    text: &'a str,
    out_index: usize,
}

/// Decode one window's logits (length `padded * label_count`; only the first `tokens.len()` rows
/// are read) into spans appended to `out[window.out_index]`.
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
