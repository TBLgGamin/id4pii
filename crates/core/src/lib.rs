#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
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

pub use anonymize::{
    IndexedVault, Placement, Rng, SurrogateStore, Vault, VaultEntry, anonymize, anonymize_into,
    anonymize_placements, anonymize_with_subs, apply_placements, deanonymize, warm_up_pools,
};
pub use detect::{Detector, PiiSpan, regex_scan};
pub use error::{Error, Result};
pub use labels::Category;
pub use redact::{RedactStyle, redact};
