//! Loading zones.
//!
//! The zone loader is responsible for maintaining up-to-date copies of the DNS
//! zones known to Nameshed.  Every zone has a configured source (e.g. zonefile,
//! DNS server, etc.) that will be monitored for changes.

#![warn(dead_code)]
#![warn(unused_variables)]

use std::{
    cmp::Ordering,
    collections::VecDeque,
    fmt,
    sync::{Arc, MutexGuard},
};

use domain::new::base::Serial;
use tokio::sync::mpsc;

use crate::{
    payload::Update,
    zone::{self, contents, Zone, ZoneContents, ZoneState},
};

mod refresh;
mod server;
mod zonefile;

pub use refresh::RefreshMonitor;

//----------- Loader -----------------------------------------------------------

/// The loader.
pub struct Loader {
    /// The refresh monitor.
    pub refresh_monitor: RefreshMonitor,

    /// A sender for updates from the loader.
    pub update_tx: mpsc::Sender<Update>,
}

impl Loader {
    /// Construct a new [`Loader`].
    pub fn new(update_tx: mpsc::Sender<Update>) -> Self {
        Self {
            refresh_monitor: RefreshMonitor::new(),
            update_tx,
        }
    }

    /// Drive this [`Loader`].
    pub async fn run(self: &Arc<Self>) {
        self.refresh_monitor.run(self).await;
    }
}

//----------- refresh() --------------------------------------------------------

/// Refresh a zone from DNS server.
///
/// The DNS server will be queried for the latest version of the zone; if a
/// local copy of this version is not already available, it will be loaded.
/// Where possible, an incremental zone transfer will be used to communicate
/// more efficiently.
pub async fn refresh<'z>(
    zone: &'z Arc<Zone>,
    source: &zone::loader::Source,
    latest: Option<Arc<contents::Uncompressed>>,
) -> (
    Result<Option<Serial>, RefreshError>,
    Option<MutexGuard<'z, ZoneState>>,
) {
    // Perform the source-specific refresh operation.
    let refresh = match source {
        // Refreshing a zone without a source is a no-op.
        zone::loader::Source::None => return (Ok(None), None),

        zone::loader::Source::Zonefile { path } => {
            let zone = zone.clone();
            let path = path.clone();
            tokio::task::spawn_blocking(move || zonefile::refresh(&zone, &path, latest))
                .await
                .unwrap()
        }

        zone::loader::Source::Server { addr } => server::refresh(zone, addr, latest).await,
    };

    // Process the result.
    let Refresh {
        uncompressed,
        mut compressed,
    } = match refresh {
        // The local copy is up-to-date.
        Ok(None) => return (Ok(None), None),

        // The local copy is outdated.
        Ok(Some(refresh)) => refresh,

        // An error occurred.
        Err(error) => return (Err(error), None),
    };

    // Integrate the results with the zone state.
    let remote_serial = uncompressed.soa.rdata.serial;
    loop {
        // TODO: A limitation in Rust's coroutine witness tracking means that
        // the more idiomatic representation of the following block forces the
        // future to '!Send'.  This is used as a workaround.

        /// An action to do after examining the zone state.
        enum Action {
            /// Compress a local copy relative to the remote copy.
            Compress(Arc<contents::Uncompressed>),
        }

        let action = 'lock: {
            // Lock the zone state.
            let mut lock = zone.data.lock().unwrap();

            // If no previous version of the zone exists (or if the zone
            // contents have been cleared since the start of the refresh),
            // insert the remote copy directly.
            let Some(contents) = &mut lock.contents else {
                lock.contents = Some(ZoneContents {
                    latest: uncompressed,
                    previous: VecDeque::new(),
                });

                return (Ok(Some(remote_serial)), Some(lock));
            };

            // Compare the _current_ latest version of the zone against the
            // remote.
            let local_serial = contents.latest.soa.rdata.serial;
            match local_serial.partial_cmp(&remote_serial) {
                Some(Ordering::Less) => {}

                Some(Ordering::Equal) => {
                    // The local copy was updated externally, and is now
                    // up-to-date with respect to the remote copy.  Stop.
                    return (Ok(None), Some(lock));
                }

                Some(Ordering::Greater) | None => {
                    // The local copy was updated externally, and is now more
                    // recent than the remote copy.  While it is possible the remote
                    // copy has also been updated, we will assume it's unchanged,
                    // and report that the remote has become outdated.
                    return (
                        Err(RefreshError::OutdatedRemote {
                            local_serial,
                            remote_serial,
                        }),
                        Some(lock),
                    );
                }
            }

            // If 'compressed' is up-to-date, use it.
            if let Some((compressed, _)) = compressed
                .take()
                .filter(|(_, l)| l.soa.rdata.serial == local_serial)
            {
                contents.latest = uncompressed;
                contents.previous.push_back(compressed);
                return (Ok(Some(remote_serial)), Some(lock));
            }

            // 'compressed' needs to be updated.
            let local = contents.latest.clone();
            break 'lock Action::Compress(local);
        };

        match action {
            Action::Compress(local) => {
                let remote = uncompressed.clone();
                compressed = tokio::task::spawn_blocking(move || {
                    Some((Arc::new(local.compress(&remote)), local))
                })
                .await
                .unwrap();
            }
        }
    }
}

/// The internal result of a zone refresh.
struct Refresh {
    /// The uncompressed remote copy.
    ///
    /// If the remote provided an uncompressed copy, this holds it verbatim.
    /// If the remote provided a compressed copy, it is uncompressed relative
    /// to the latest local copy and stored here.
    uncompressed: Arc<contents::Uncompressed>,

    /// A compressed version of the local copy.
    ///
    /// This is a compressed version of the local copy -- the latest version
    /// known when the refresh began.  This will forward the local copy to the
    /// remote copy.  It holds the corresponding uncompressed local copy.
    compressed: Option<(Arc<contents::Compressed>, Arc<contents::Uncompressed>)>,
}

//----------- reload() ---------------------------------------------------------

/// Reload a zone.
///
/// The complete contents of the zone will be loaded from the source, without
/// relying on the local copy at all.  If this results in a new version of the
/// zone, it is registered in the zone storage; otherwise, the loaded data is
/// compared to the local copy of the same zone version.  If an inconsistency is
/// detected, an error is returned, and the zone storage is unchanged.
pub async fn reload<'z>(
    zone: &'z Arc<Zone>,
    source: &zone::loader::Source,
) -> (
    Result<Option<Serial>, ReloadError>,
    Option<MutexGuard<'z, ZoneState>>,
) {
    // Perform the source-specific reload operation.
    let reload = match source {
        // Reloading a zone without a source is a no-op.
        zone::loader::Source::None => return (Ok(None), None),

        zone::loader::Source::Zonefile { path } => {
            let zone = zone.clone();
            let path = path.clone();
            tokio::task::spawn_blocking(move || zonefile::load(&zone, &path))
                .await
                .unwrap()
                .map_err(ReloadError::Zonefile)
        }

        zone::loader::Source::Server { addr } => {
            server::axfr(zone, addr).await.map_err(ReloadError::Axfr)
        }
    };

    // Process the result.
    let remote = match reload {
        Ok(remote) => Arc::new(remote),
        Err(error) => return (Err(error), None),
    };

    // Integrate the results with the zone state.
    let remote_serial = remote.soa.rdata.serial;
    let mut compressed: Option<(Arc<contents::Compressed>, Arc<contents::Uncompressed>)> = None;
    let mut local_match = None;
    loop {
        // TODO: A limitation in Rust's coroutine witness tracking means that
        // the more idiomatic representation of the following block forces the
        // future to '!Send'.  This is used as a workaround.

        /// An action to do after examining the zone state.
        enum Action {
            /// Compare a local copy to the remote copy.
            Compare(Arc<contents::Uncompressed>),

            /// Compress a local copy relative to the remote copy.
            Compress(Arc<contents::Uncompressed>),
        }

        let action = 'lock: {
            // Lock the zone state.
            let mut lock = zone.data.lock().unwrap();

            // If no previous version of the zone exists (or if the zone
            // contents have been cleared since the start of the refresh),
            // insert the remote copy directly.
            let Some(contents) = &mut lock.contents else {
                lock.contents = Some(ZoneContents {
                    latest: remote,
                    previous: VecDeque::new(),
                });

                return (Ok(Some(remote_serial)), Some(lock));
            };

            // Compare the _current_ latest version of the zone against the remote.
            let local_serial = contents.latest.soa.rdata.serial;
            match local_serial.partial_cmp(&remote_serial) {
                Some(Ordering::Less) => {}

                Some(Ordering::Equal) => {
                    // The local copy was updated externally, and is now up-to-date
                    // with respect to the remote copy.  Compare the contents of the
                    // two.

                    // If the two have already been compared, end now.
                    if local_match
                        .take()
                        .is_some_and(|l| Arc::ptr_eq(&l, &contents.latest))
                    {
                        return (Ok(None), Some(lock));
                    }

                    // Compare 'latest' and 'remote'.
                    let latest = contents.latest.clone();
                    break 'lock Action::Compare(latest);
                }

                Some(Ordering::Greater) | None => {
                    // The local copy was updated externally, and is now more
                    // recent than the remote copy.  While it is possible the remote
                    // copy has also been updated, we will assume it's unchanged,
                    // and report that the remote has become outdated.
                    return (
                        Err(ReloadError::OutdatedRemote {
                            local_serial,
                            remote_serial,
                        }),
                        Some(lock),
                    );
                }
            }

            // If 'compressed' is up-to-date w.r.t. the current local copy, use it.
            if let Some((compressed, _)) = compressed
                .take()
                .filter(|(_, l)| l.soa.rdata.serial == local_serial)
            {
                contents.latest = remote;
                contents.previous.push_back(compressed);
                return (Ok(Some(remote_serial)), Some(lock));
            }

            let local = contents.latest.clone();
            break 'lock Action::Compress(local);
        };

        match action {
            Action::Compare(latest) => {
                let latest_copy = latest.clone();
                let remote_copy = remote.clone();
                let matches =
                    tokio::task::spawn_blocking(move || remote_copy.eq_unsigned(&latest_copy))
                        .await
                        .unwrap();

                if matches {
                    local_match = Some(latest);
                    continue;
                } else {
                    return (Err(ReloadError::Inconsistent), None);
                }
            }

            Action::Compress(local) => {
                // 'compressed' needs to be updated relative to the current local copy.
                let remote = remote.clone();
                compressed = tokio::task::spawn_blocking(move || {
                    Some((Arc::new(local.compress(&remote)), local))
                })
                .await
                .unwrap();
                continue;
            }
        }
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
