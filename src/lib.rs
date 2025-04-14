//! Rotonda
#![allow(renamed_and_removed_lints)]
#![allow(clippy::unknown_clippy_lints)]
#![allow(dead_code)]
#![allow(unused_variables)]
pub mod api;
pub mod cli;
mod common;
mod comms;
pub mod config;
mod error;
mod http;
pub mod log;
pub mod manager;
pub mod metrics;
mod payload;
mod targets;
mod tokio;
mod tracing;
mod units;
mod zonemaintenance;

#[cfg(test)]
pub mod tests;
