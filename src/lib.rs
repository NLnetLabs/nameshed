//! Rotonda
#![allow(renamed_and_removed_lints)]
#![allow(clippy::unknown_clippy_lints)]
#![allow(dead_code)]
#![allow(unused_variables)]
mod api;
pub mod cli;
mod common;
mod comms;
pub mod config;
mod error;
mod http;
pub mod log;
pub mod manager;
mod metrics;
mod payload;
mod targets;
mod tokio;
mod tracing;
mod units;
mod zonemaintenance;

mod new;

#[cfg(test)]
mod tests;
