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

#[derive(Clone, Copy, Debug)]
pub(crate) enum Coalesce {
    Off,

    UpTo(usize),
}

#[derive(Clone)]
pub(crate) struct DetectorService {
    tx: SyncSender<Job>,
}

impl DetectorService {
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

    fn marker(pos: usize) -> PiiSpan {
        PiiSpan {
            category: Category::PrivatePerson,
            start: pos,
            end: pos + 1,
            text: "x".to_string(),
            score: 1.0,
        }
    }

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

        assert_eq!(a.len(), 2);
        assert_eq!(a[0][0].start, 0);
        assert_eq!(a[1][0].start, 1);

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
