use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result};

use crate::{Detector, PiiSpan};

pub(crate) type SpansResult = std::result::Result<Vec<Vec<PiiSpan>>, String>;

struct Job {
    texts: Vec<String>,
    min_score: f32,
    reply: SyncSender<SpansResult>,
}

/// How the model thread groups queued jobs into a single inference.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Coalesce {
    /// Run each submitted job as its own inference, in submission order.
    /// Preserves a caller's own batching and backpressure (the corpus
    /// pipeline relies on this to stream a multi-GB corpus and to mint
    /// shared-vault surrogates in record order).
    Off,
    /// Opportunistically merge up to `cap` queued jobs that share a
    /// `min_score` into one inference, coalescing concurrent load with no
    /// added latency for a lone request (the HTTP server).
    UpTo(usize),
}

/// A [`Detector`] living on one dedicated thread, fed by a bounded queue.
///
/// This is the single place the "exactly one inference thread" rule (the ORT
/// intra-op deadlock guard) is enforced. Both the HTTP server and the corpus
/// pipeline submit text batches here; the only difference is the [`Coalesce`]
/// policy. Submission is request/reply over a per-job channel, so callers can
/// either block ([`Self::submit`]) or fire-and-collect-later
/// ([`Self::submit_async`]) while staying in FIFO order.
#[derive(Clone)]
pub(crate) struct DetectorService {
    tx: SyncSender<Job>,
}

impl DetectorService {
    /// Spawn the model thread. The returned [`JoinHandle`] completes once every
    /// clone of the service is dropped (closing the queue); a streaming caller
    /// drops its handle and joins to drain, a long-lived server just keeps it.
    pub(crate) fn spawn(
        detector: Detector,
        coalesce: Coalesce,
        queue_depth: usize,
    ) -> Result<(Self, JoinHandle<()>)> {
        let (tx, rx) = sync_channel::<Job>(queue_depth.max(1));
        let handle = thread::Builder::new()
            .name("id4pii-detect".to_string())
            .spawn(move || model_thread(detector, &rx, coalesce))
            .context("failed to spawn detector thread")?;
        Ok((Self { tx }, handle))
    }

    /// Queue a batch and return a receiver for its spans without blocking.
    pub(crate) fn submit_async(
        &self,
        texts: Vec<String>,
        min_score: f32,
    ) -> std::result::Result<Receiver<SpansResult>, String> {
        let (reply, rx) = sync_channel::<SpansResult>(1);
        self.tx
            .send(Job {
                texts,
                min_score,
                reply,
            })
            .map_err(|_| "detector unavailable".to_string())?;
        Ok(rx)
    }

    /// Queue a batch and block until its spans are ready.
    pub(crate) fn submit(&self, texts: Vec<String>, min_score: f32) -> SpansResult {
        self.submit_async(texts, min_score)?
            .recv()
            .map_err(|_| "detector dropped the request".to_string())?
    }
}

fn model_thread(mut detector: Detector, rx: &Receiver<Job>, coalesce: Coalesce) {
    let mut carry: Option<Job> = None;
    loop {
        let first = match carry.take() {
            Some(job) => job,
            None => match rx.recv() {
                Ok(job) => job,
                Err(_) => break,
            },
        };
        let min_score = first.min_score;
        let mut jobs = vec![first];
        if let Coalesce::UpTo(cap) = coalesce {
            while jobs.len() < cap {
                match rx.try_recv() {
                    // Same threshold: fold into this inference.
                    Ok(job) if job.min_score.to_bits() == min_score.to_bits() => jobs.push(job),
                    // Different threshold: run what we have, carry this over.
                    Ok(job) => {
                        carry = Some(job);
                        break;
                    }
                    Err(_) => break,
                }
            }
        }
        run_jobs(&mut detector, jobs, min_score);
    }
}

fn run_jobs(detector: &mut Detector, jobs: Vec<Job>, min_score: f32) {
    let texts: Vec<&str> = jobs
        .iter()
        .flat_map(|job| job.texts.iter().map(String::as_str))
        .collect();
    let outcome = detector.detect_batch(&texts, min_score);
    drop(texts);

    match outcome {
        Ok(all) => {
            let mut iter = all.into_iter();
            for job in jobs {
                let spans: Vec<Vec<PiiSpan>> = iter.by_ref().take(job.texts.len()).collect();
                let _ = job.reply.send(Ok(spans));
            }
        }
        Err(err) => {
            let message = err.to_string();
            for job in jobs {
                let _ = job.reply.send(Err(message.clone()));
            }
        }
    }
}
