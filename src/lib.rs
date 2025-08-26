//! Nameshed
#![allow(dead_code)]

mod common;
mod comms;
pub mod config;
pub mod log;
pub mod manager;
pub mod metrics;
mod payload;
mod targets;
mod units;
mod zonemaintenance;

#[cfg(test)]
pub mod tests;
