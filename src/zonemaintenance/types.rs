use std::collections::HashMap;

use serde::Serializer;
use tokio::time::Instant;

use core::time::Duration;
use domain::tsig::{Algorithm, Key, KeyName};

//------------ Type Aliases --------------------------------------------------

/// A store of TSIG keys index by key name and algorithm.
#[allow(dead_code)]
pub type ZoneMaintainerKeyStore = HashMap<(KeyName, Algorithm), Key>;

//------------ ZoneRefreshMetrics --------------------------------------------

pub fn instant_to_duration_secs(instant: Instant) -> u64 {
    match Instant::now().checked_duration_since(instant) {
        Some(d) => d.as_secs(),
        None => 0,
    }
}

pub fn serialize_instant_as_duration_secs<S>(
    instant: &Instant,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_u64(instant_to_duration_secs(*instant))
}

pub fn serialize_duration_as_secs<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_u64(duration.as_secs())
}

pub fn serialize_opt_duration_as_secs<S>(
    instant: &Option<Duration>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match instant {
        Some(v) => serialize_duration_as_secs(v, serializer),
        None => serializer.serialize_str("null"),
    }
}
