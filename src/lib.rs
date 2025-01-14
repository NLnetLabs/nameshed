//! A DNS primary server – library crate.

pub use crate::config::Config;
pub use crate::error::ExitError;

pub mod archive;
pub mod config;
pub mod error;
pub mod operation;
pub mod process;
pub mod self_signing_zone;
pub mod zonemaintenance;
