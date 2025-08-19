//! Rotonda
#![allow(renamed_and_removed_lints)]
#![allow(clippy::unknown_clippy_lints)]
#![allow(clippy::uninlined_format_args)]
#![allow(dead_code)]
#![allow(unused_variables)]

pub mod api;
pub mod cli;
mod common;
mod comms;
pub mod config;
pub mod http;
pub mod log;
pub mod manager;
pub mod metrics;
mod payload;
mod targets;
mod units;
mod zonemaintenance;

pub mod loader;
pub mod zone;

pub mod path_monitor;

#[cfg(test)]
pub mod tests;
