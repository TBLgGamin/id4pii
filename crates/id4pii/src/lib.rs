#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::module_name_repetitions
)]

mod anonymize;
mod detect;
mod error;
pub mod eval;
mod labels;
pub mod model_dir;
pub mod model_fetch;
pub mod paths;
mod redact;

pub mod cli;
pub mod corpus;
#[cfg(windows)]
pub mod daemon;
mod detector_service;
pub mod document;
#[cfg(windows)]
pub mod install;
pub mod logging;
pub mod model_setup;
mod ops;
pub mod progress;
pub mod serve;

pub use anonymize::{
    IndexedVault, Placement, Rng, SurrogateStore, Vault, VaultEntry, anonymize, anonymize_into,
    anonymize_placements, anonymize_with_subs, apply_placements, deanonymize, warm_up_pools,
};
pub use detect::{Detector, PiiSpan, regex_scan};
pub use error::{Error, Result};
pub use labels::Category;
pub use redact::{RedactStyle, redact};
