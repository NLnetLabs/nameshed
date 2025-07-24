//! Loading zones.
//!
//! The zone loader is responsible for maintaining up-to-date copies of the DNS
//! zones known to Nameshed.  Every zone has a configured source (e.g. zonefile,
//! DNS server, etc.) that will be monitored for changes.

#![warn(dead_code)]
#![warn(unused_variables)]

use std::sync::Arc;

use domain::new::base::Serial;

use crate::zone::{self, Zone};

mod server;
mod zonefile;

//------------------------------------------------------------------------------

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
