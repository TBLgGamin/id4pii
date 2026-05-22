use std::path::Path;

use anyhow::{Context, Result};
use id4pii_core::{Detector, Rng, Vault, anonymize_into, deanonymize};

pub(crate) struct Session {
    detector: Detector,
    vault: Vault,
    rng: Rng,
}

impl Session {
    pub(crate) fn load(model: &Path, model_file: &str, threads: usize) -> Result<Self> {
        let detector =
            Detector::load(model, model_file, threads).context("failed to load model")?;
        Ok(Self {
            detector,
            vault: Vault::default(),
            rng: Rng::from_entropy(),
        })
    }

    pub(crate) fn anonymize(&mut self, text: &str) -> Result<(String, usize)> {
        let spans = self.detector.detect(text).context("detection failed")?;
        let count = spans.len();
        let result = anonymize_into(text, &spans, &mut self.rng, &mut self.vault);
        Ok((result, count))
    }

    pub(crate) fn deanonymize(&self, text: &str) -> String {
        deanonymize(text, &self.vault)
    }
}
