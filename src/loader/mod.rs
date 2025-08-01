//! Loading zones.
//!
//! The zone loader is responsible for maintaining up-to-date copies of the DNS
//! zones known to Nameshed.  Every zone has a configured source (e.g. zonefile,
//! DNS server, etc.) that will be monitored for changes.

#![warn(dead_code)]
#![warn(unused_variables)]

use std::{fmt, sync::Arc};

use domain::new::base::Serial;

use crate::zone::{self, contents, Zone};

mod refresh;
mod server;
mod zonefile;

pub use refresh::RefreshMonitor;

//----------- Loader -----------------------------------------------------------

/// The loader.
pub struct Loader {
    /// The refresh monitor.
    pub refresh_monitor: RefreshMonitor,
}

//----------- refresh() --------------------------------------------------------

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

//----------- reload() ---------------------------------------------------------

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
#[derive(Debug)]
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

impl std::error::Error for RefreshError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::OutdatedRemote { .. } => None,
            Self::Ixfr(error) => Some(error),
            Self::Axfr(error) => Some(error),
            Self::Zonefile(error) => Some(error),
            Self::MergeIxfr(error) => Some(error),
            Self::ForwardIxfr(error) => Some(error),
        }
    }
}

impl fmt::Display for RefreshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RefreshError::OutdatedRemote {
                local_serial,
                remote_serial,
            } => {
                write!(f, "the source of the zone is reporting an outdated SOA ({remote_serial}, while the latest local copy is {local_serial})")
            }
            RefreshError::Ixfr(error) => {
                write!(f, "the IXFR failed: {error}")
            }
            RefreshError::Axfr(error) => {
                write!(f, "the AXFR failed: {error}")
            }
            RefreshError::Zonefile(error) => {
                write!(f, "the zonefile could not be loaded: {error}")
            }
            RefreshError::MergeIxfr(error) => {
                write!(f, "the IXFR was internally inconsistent: {error}")
            }
            RefreshError::ForwardIxfr(error) => {
                write!(
                    f,
                    "the IXFR was inconsistent with the local zone contents: {error}"
                )
            }
        }
    }
}

//--- Conversion

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
#[derive(Debug)]
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

impl std::error::Error for ReloadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReloadError::OutdatedRemote { .. } => None,
            ReloadError::Inconsistent => None,
            ReloadError::Axfr(error) => Some(error),
            ReloadError::Zonefile(error) => Some(error),
        }
    }
}

impl fmt::Display for ReloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReloadError::OutdatedRemote {
                local_serial,
                remote_serial,
            } => {
                write!(f, "the source of the zone is reporting an outdated SOA ({remote_serial}, while the latest local copy is {local_serial})")
            }
            ReloadError::Inconsistent => write!(f, "the local and remote copies are inconsistent"),
            ReloadError::Axfr(error) => write!(f, "the AXFR failed: {error}"),
            ReloadError::Zonefile(error) => write!(f, "the zonefile could not be loaded: {error}"),
        }
    }
}

//--- Conversion

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
