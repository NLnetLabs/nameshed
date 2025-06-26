//! Rotonda
#![allow(renamed_and_removed_lints)]
#![allow(clippy::unknown_clippy_lints)]
#![allow(dead_code)]
#![allow(unused_variables)]

mod common;
mod comms;
pub mod http;
pub mod log;
pub mod manager;
pub mod metrics;
mod payload;
mod targets;
mod units;
mod zonemaintenance;

#[cfg(test)]
pub mod tests;
