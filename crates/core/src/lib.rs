#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::module_name_repetitions
)]

mod anonymize;
mod detector;
mod error;
mod labels;
mod redact;

pub use anonymize::{Rng, Vault, VaultEntry, anonymize, anonymize_into, deanonymize};
pub use detector::{Detector, PiiSpan};
pub use error::{Error, Result};
pub use labels::Category;
pub use redact::{RedactStyle, redact};
