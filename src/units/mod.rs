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

pub mod zone_loader;
pub mod zone_server;
pub mod zone_signer;

//------------ Unit ----------------------------------------------------------

use crate::comms::Gate;
use crate::manager::{Component, WaitPoint};
use serde::Deserialize;

/// The fundamental entity for data processing.
#[allow(clippy::enum_variant_names)]
#[derive(Debug)]
pub enum Unit {
    ZoneLoader(zone_loader::ZoneLoaderUnit),

    ZoneServer(zone_server::ZoneServerUnit),

    ZoneSigner(zone_signer::ZoneSignerUnit),
}

impl Unit {
    pub async fn run(self, component: Component, gate: Gate, waitpoint: WaitPoint) {
        let _ = match self {
            Unit::ZoneLoader(unit) => unit.run(component, gate, waitpoint).await,
            Unit::ZoneServer(unit) => unit.run(component, gate, waitpoint).await,
            Unit::ZoneSigner(unit) => unit.run(component, gate, waitpoint).await,
        };
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Unit::ZoneLoader(_) => "zone-loader",
            Unit::ZoneServer(_) => "zone-server",
            Unit::ZoneSigner(_) => "zone-signer",
        }
    }
}
