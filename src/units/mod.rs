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

pub mod key_manager;
pub mod zone_server;
pub mod zone_signer;

//------------ Unit ----------------------------------------------------------

use crate::manager::Component;
use serde::Deserialize;

/// The fundamental entity for data processing.
#[allow(clippy::enum_variant_names)]
#[derive(Debug)]
pub enum Unit {
    KeyManager(key_manager::KeyManagerUnit),

    ZoneServer(zone_server::ZoneServerUnit),

    ZoneSigner(zone_signer::ZoneSignerUnit),
}

impl Unit {
    pub async fn run(self, component: Component) {
        let _ = match self {
            Unit::KeyManager(unit) => unit.run(component).await,
            Unit::ZoneServer(unit) => unit.run(component).await,
            Unit::ZoneSigner(unit) => unit.run(component).await,
        };
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Unit::KeyManager(_) => "key-manager",
            Unit::ZoneServer(_) => "zone-server",
            Unit::ZoneSigner(_) => "zone-signer",
        }
    }
}
