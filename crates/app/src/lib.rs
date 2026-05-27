#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]

pub mod cli;
#[cfg(windows)]
pub mod guard;
#[cfg(windows)]
pub mod install;
pub mod logging;
pub mod model_setup;
pub mod progress;
pub mod serve;
