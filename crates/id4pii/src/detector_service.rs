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
    /// Opportunistically merge up to `cap` queued jobs into one inference,
    /// coalescing concurrent load with no added latency for a lone request
    /// (the HTTP server).
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
    /// Spawn the model thread owning `detector`. The returned [`JoinHandle`]
    /// completes once every clone of the service is dropped (closing the
    /// queue); a streaming caller drops its handle and joins to drain, a
    /// long-lived server just keeps it.
    pub(crate) fn spawn(
        mut detector: Detector,
        coalesce: Coalesce,
        queue_depth: usize,
    ) -> Result<(Self, JoinHandle<()>)> {
        Self::spawn_with(
            move |texts, min_score| {
                detector
                    .detect_batch(texts, min_score)
                    .map_err(|e| e.to_string())
            },
            coalesce,
            queue_depth,
        )
    }

    /// Spawn over an arbitrary "texts + threshold -> spans" function. The
    /// production constructor wraps a [`Detector`]; tests inject a fake.
    fn spawn_with<F>(
        run: F,
        coalesce: Coalesce,
        queue_depth: usize,
    ) -> Result<(Self, JoinHandle<()>)>
    where
        F: FnMut(&[&str], f32) -> SpansResult + Send + 'static,
    {
        let (tx, rx) = sync_channel::<Job>(queue_depth.max(1));
        let handle = thread::Builder::new()
            .name("id4pii-detect".to_string())
            .spawn(move || model_thread(run, &rx, coalesce))
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

fn model_thread<F>(mut run: F, rx: &Receiver<Job>, coalesce: Coalesce)
where
    F: FnMut(&[&str], f32) -> SpansResult,
{
    while let Ok(first) = rx.recv() {
        let mut jobs = vec![first];
        if let Coalesce::UpTo(cap) = coalesce {
            // Drain whatever else is already queued into one inference.
            // Coalesced jobs share a threshold by construction: `serve` always
            // submits min_score = 0.0 (and filters per-request afterwards), and
            // the corpus pipeline uses Coalesce::Off — so the batch runs at the
            // first job's threshold.
            while jobs.len() < cap {
                match rx.try_recv() {
                    Ok(job) => jobs.push(job),
                    Err(_) => break,
                }
            }
        }
        run_jobs(&mut run, jobs);
    }
}

fn run_jobs<F>(run: &mut F, jobs: Vec<Job>)
where
    F: FnMut(&[&str], f32) -> SpansResult,
{
    let min_score = jobs[0].min_score;
    let texts: Vec<&str> = jobs
        .iter()
        .flat_map(|job| job.texts.iter().map(String::as_str))
        .collect();
    let outcome = run(&texts, min_score);
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
            for job in jobs {
                let _ = job.reply.send(Err(err.clone()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Coalesce, DetectorService, Job, SpansResult, run_jobs};
    use crate::{Category, PiiSpan};
    use std::sync::mpsc::sync_channel;

    // Encode the batch position in `start` (an integer, so assertions avoid
    // float comparison) so a reply can be traced back to the exact texts it
    // should have received.
    fn marker(pos: usize) -> PiiSpan {
        PiiSpan {
            category: Category::PrivatePerson,
            start: pos,
            end: pos + 1,
            text: "x".to_string(),
            score: 1.0,
        }
    }

    // One single-span Vec per input text, marked by its position in the
    // (possibly coalesced) batch. The `Result` return matches the run-fn seam.
    #[allow(clippy::unnecessary_wraps)]
    fn positional_run(texts: &[&str], _min_score: f32) -> SpansResult {
        Ok(texts
            .iter()
            .enumerate()
            .map(|(i, _)| vec![marker(i)])
            .collect())
    }

    #[test]
    fn run_jobs_splits_replies_by_job_text_count() {
        let (a_tx, a_rx) = sync_channel(1);
        let (b_tx, b_rx) = sync_channel(1);
        let jobs = vec![
            Job {
                texts: vec!["a".to_string(), "b".to_string()],
                min_score: 0.0,
                reply: a_tx,
            },
            Job {
                texts: vec!["c".to_string()],
                min_score: 0.0,
                reply: b_tx,
            },
        ];

        run_jobs(&mut positional_run, jobs);

        let a = a_rx.recv().unwrap().unwrap();
        let b = b_rx.recv().unwrap().unwrap();
        // Job A asked for 2 texts -> the first 2 results (positions 0,1).
        assert_eq!(a.len(), 2);
        assert_eq!(a[0][0].start, 0);
        assert_eq!(a[1][0].start, 1);
        // Job B asked for 1 text -> the next result (position 2), no bleed-over.
        assert_eq!(b.len(), 1);
        assert_eq!(b[0][0].start, 2);
    }

    #[test]
    fn run_jobs_propagates_error_to_every_job() {
        let (a_tx, a_rx) = sync_channel(1);
        let (b_tx, b_rx) = sync_channel(1);
        let jobs = vec![
            Job {
                texts: vec!["a".to_string()],
                min_score: 0.0,
                reply: a_tx,
            },
            Job {
                texts: vec!["b".to_string()],
                min_score: 0.0,
                reply: b_tx,
            },
        ];

        run_jobs(&mut |_texts, _min_score| Err("boom".to_string()), jobs);

        assert_eq!(a_rx.recv().unwrap().unwrap_err(), "boom");
        assert_eq!(b_rx.recv().unwrap().unwrap_err(), "boom");
    }

    #[test]
    fn submit_round_trips_through_the_thread() {
        let (service, handle) =
            DetectorService::spawn_with(positional_run, Coalesce::Off, 4).unwrap();
        let out = service
            .submit(vec!["a".to_string(), "b".to_string()], 0.0)
            .unwrap();
        assert_eq!(out.len(), 2);
        drop(service);
        handle.join().unwrap();
    }
}
