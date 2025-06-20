#![allow(unused_imports)]

//! Data processing units.
//!
//! Rotonda provides the means for flexible data processing through
//! interconnected entities called _units._ Each unit produces a constantly
//! updated data set. Other units can subscribe to updates from these sets.
//! Alternatively, they can produce their own data set from external input.
//! Different types of units exist that perform different tasks. They can
//! all be plugged together all kinds of ways.
//!
//! This module contains all the units currently available. It provides
//! access to them via a grand enum `Unit` that contains all unit types as
//! variants.
//!
//! Units can be created from configuration via serde deserialization. They
//! are started by spawning them into an async runtime and then just keep
//! running there.

//------------ Sub-modules ---------------------------------------------------

mod key_manager;
mod zone_loader;
mod zone_server;
mod zone_signer;

//------------ Unit ----------------------------------------------------------

use crate::comms::Gate;
use crate::manager::{Component, WaitPoint};
use serde::Deserialize;

/// The fundamental entity for data processing.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type")]
pub enum Unit {
    #[serde(rename = "key-manager")]
    KeyManager(key_manager::KeyManagerUnit),

    #[serde(rename = "zone-loader")]
    ZoneLoader(zone_loader::ZoneLoaderUnit),

    #[serde(rename = "zone-server")]
    ZoneServer(zone_server::ZoneServerUnit),

    #[serde(rename = "zone-signer")]
    ZoneSigner(zone_signer::ZoneSignerUnit),
}

impl Unit {
    pub async fn run(self, component: Component, gate: Gate, waitpoint: WaitPoint) {
        let _ = match self {
            Unit::KeyManager(unit) => unit.run(component, gate, waitpoint).await,
            Unit::ZoneLoader(unit) => unit.run(component, gate, waitpoint).await,
            Unit::ZoneServer(unit) => unit.run(component, gate, waitpoint).await,
            Unit::ZoneSigner(unit) => unit.run(component, gate, waitpoint).await,
        };
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Unit::KeyManager(_) => "key-manager",
            Unit::ZoneLoader(_) => "zone-loader",
            Unit::ZoneServer(_) => "zone-server",
            Unit::ZoneSigner(_) => "zone-signer",
        }
    }
}
