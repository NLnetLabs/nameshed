//! Loading zones.
//!
//! The zone loader is responsible for maintaining up-to-date copies of the DNS
//! zones known to Nameshed.  Every zone has a configured source (e.g. zonefile,
//! DNS server, etc.) that will be monitored for changes.

#![warn(dead_code)]
#![warn(unused_variables)]

use std::sync::Arc;

use domain::new::base::Serial;

use crate::zone::{self, contents, Zone};

mod server;
mod zonefile;

//------------------------------------------------------------------------------

/// Refresh a zone from DNS server.
///
/// The DNS server will be queried for the latest version of the zone; if a
/// local copy of this version is not already available, it will be loaded.
/// Where possible, an incremental zone transfer will be used to communicate
/// more efficiently.
pub async fn refresh(
    zone: &Arc<Zone>,
    source: &zone::loader::Source,
    latest: Option<Arc<contents::Uncompressed>>,
) -> Result<Option<Serial>, RefreshError> {
    match source {
        // Refreshing a zone without a source is a no-op.
        zone::loader::Source::None => Ok(None),

        zone::loader::Source::Zonefile { path } => {
            let zone = zone.clone();
            let path = path.clone();
            tokio::task::spawn_blocking(move || zonefile::refresh(&zone, &path, latest))
                .await
                .unwrap()
        }

        zone::loader::Source::Server { addr } => server::refresh(zone, addr, latest).await,
    }
}

/// Reload a zone.
///
/// The complete contents of the zone will be loaded from the source, without
/// relying on the local copy at all.  If this results in a new version of the
/// zone, it is registered in the zone storage; otherwise, the loaded data is
/// compared to the local copy of the same zone version.  If an inconsistency is
/// detected, an error is returned, and the zone storage is unchanged.
pub async fn reload(
    zone: &Arc<Zone>,
    source: &zone::loader::Source,
) -> Result<Option<Serial>, ReloadError> {
    match source {
        // Reloading a zone without a source is a no-op.
        zone::loader::Source::None => Ok(None),

        zone::loader::Source::Zonefile { path } => {
            let zone = zone.clone();
            let path = path.clone();
            tokio::task::spawn_blocking(move || zonefile::reload(&zone, &path))
                .await
                .unwrap()
        }

        zone::loader::Source::Server { addr } => server::reload(zone, addr).await,
    }
}

//============ Errors ==========================================================

//----------- RefreshError -----------------------------------------------------

/// An error when refreshing a zone.
pub enum RefreshError {
    /// The source of the zone appears to be outdated.
    OutdatedRemote {
        /// The SOA serial of the local copy.
        local_serial: Serial,

        /// The SOA serial of the remote copy.
        remote_serial: Serial,
    },

    /// An IXFR from the server failed.
    Ixfr(server::IxfrError),

    /// An AXFR from the server failed.
    Axfr(server::AxfrError),

    /// The zonefile could not be loaded.
    Zonefile(zonefile::Error),

    /// An IXFR's diff was internally inconsistent.
    MergeIxfr(contents::MergeError),

    /// An IXFR's diff was not consistent with the local copy.
    ForwardIxfr(contents::ForwardError),
}

impl From<server::IxfrError> for RefreshError {
    fn from(v: server::IxfrError) -> Self {
        Self::Ixfr(v)
    }
}

impl From<server::AxfrError> for RefreshError {
    fn from(v: server::AxfrError) -> Self {
        Self::Axfr(v)
    }
}

impl From<zonefile::Error> for RefreshError {
    fn from(v: zonefile::Error) -> Self {
        Self::Zonefile(v)
    }
}

impl From<contents::MergeError> for RefreshError {
    fn from(v: contents::MergeError) -> Self {
        Self::MergeIxfr(v)
    }
}

impl From<contents::ForwardError> for RefreshError {
    fn from(v: contents::ForwardError) -> Self {
        Self::ForwardIxfr(v)
    }
}

//----------- ReloadError ------------------------------------------------------

/// An error when reloading a zone.
pub enum ReloadError {
    /// The source of the zone appears to be outdated.
    OutdatedRemote {
        /// The SOA serial of the local copy.
        local_serial: Serial,

        /// The SOA serial of the remote copy.
        remote_serial: Serial,
    },

    /// The local and remote copies have different contents.
    Inconsistent,

    /// An AXFR from the server failed.
    Axfr(server::AxfrError),

    /// The zonefile could not be loaded.
    Zonefile(zonefile::Error),
}

impl From<server::AxfrError> for ReloadError {
    fn from(value: server::AxfrError) -> Self {
        Self::Axfr(value)
    }
}

impl From<zonefile::Error> for ReloadError {
    fn from(v: zonefile::Error) -> Self {
        Self::Zonefile(v)
    }
}
