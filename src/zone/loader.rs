//! Zone-specific loader state.

use std::{
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use camino::Utf8Path;

use crate::loader::{self, Loader, RefreshMonitor};

use super::{
    contents::{SoaRecord, Uncompressed},
    Zone, ZoneState,
};

//----------- LoaderState ------------------------------------------------------

/// State for loading new versions of a zone.
#[derive(Debug, Default)]
pub struct LoaderState {
    /// The source of the zone.
    pub source: Source,

    /// The refresh timer state of the zone.
    pub refresh_timer: RefreshTimerState,

    /// Ongoing and enqueued refreshes of the zone.
    pub refreshes: Option<Refreshes>,
    //
    // TODO:
    // - File monitoring state
}

impl LoaderState {
    /// Set the source of this zone.
    pub fn set_source(state: &mut ZoneState, zone: Arc<Zone>, source: Source) {
        // TODO: Log
        //
        // TODO: Should we enqueue a reload now, or do we leave that to the
        //   caller?  Issuing reloads during the initialization of a zone might
        //   not be the best idea.
        //
        // TODO: Should we _schedule_ refreshes now either?

        let _ = zone;
        state.loader.source = source;
    }

    /// Enqueue a refresh of this zone.
    ///
    /// If the zone is not being refreshed already, a new refresh will be
    /// initiated.  Otherwise, a refresh will be enqueued; if one is enqueued
    /// already, the two will be merged.  If `reload` is true, the refresh will
    /// verify the local copy of the zone by loading the entire zone from
    /// scratch.
    ///
    /// # Standards
    ///
    /// Complies with [RFC 1996, section 4.4], when this is used to enqueue a
    /// refresh in response to a `QTYPE=SOA` NOTIFY message.
    ///
    /// > 4.4. A slave which receives a valid NOTIFY should defer action on any
    /// > subsequent NOTIFY with the same \<QNAME,QCLASS,QTYPE\> until it has
    /// > completed the transaction begun by the first NOTIFY.  This duplicate
    /// > rejection is necessary to avoid having multiple notifications lead to
    /// > pummeling the master server.
    ///
    /// [RFC 1996, section 4.4]: https://datatracker.ietf.org/doc/html/rfc1996#section-4
    pub fn enqueue_refresh(
        state: &mut ZoneState,
        zone: &Arc<Zone>,
        reload: bool,
        loader: &Arc<Loader>,
    ) {
        // TODO: Log

        let refresh = match reload {
            false => EnqueuedRefresh::Refresh,
            true => EnqueuedRefresh::Reload,
        };

        // Determine whether a refresh is ongoing.
        if let Some(refreshes) = &mut state.loader.refreshes {
            // There is an ongoing refresh.  Enqueue a new one, which will
            // start when the ongoing one finishes.  If a refresh is already
            // enqueued, the two will be merged.
            refreshes.enqueue(refresh);
        } else {
            // Start this refresh immediately.
            Self::start(state, zone.clone(), refresh, loader.clone());
        }
    }

    /// Start an enqueued refresh.
    fn start(
        state: &mut ZoneState,
        zone: Arc<Zone>,
        refresh: EnqueuedRefresh,
        loader: Arc<Loader>,
    ) {
        let start = Instant::now();
        let source = state.loader.source.clone();
        let ongoing = match refresh {
            EnqueuedRefresh::Refresh => {
                let latest = state
                    .contents
                    .as_ref()
                    .map(|contents| contents.latest.clone());
                let handle = tokio::task::spawn(Self::refresh(zone, start, source, latest, loader));
                OngoingRefresh::Refresh { start, handle }
            }
            EnqueuedRefresh::Reload => {
                let handle = tokio::task::spawn(Self::reload(zone, start, source, loader));
                OngoingRefresh::Reload { start, handle }
            }
        };
        state.loader.refreshes = Some(Refreshes::new(ongoing));
    }

    /// Refresh this zone.
    async fn refresh(
        zone: Arc<Zone>,
        start: Instant,
        source: Source,
        latest: Option<Arc<Uncompressed>>,
        loader: Arc<Loader>,
    ) {
        // Perform the source-specific reload into the zone contents.
        let (result, lock) = loader::refresh(&zone, &source, latest).await;

        // Try to re-use a recent lock from the reload.
        let mut lock = lock.unwrap_or_else(|| zone.data.lock().unwrap());
        let state = &mut *lock;

        // Update the zone refresh timers.
        let soa = state.contents.as_ref().map(|contents| &contents.latest.soa);
        if result.is_ok() {
            state
                .loader
                .refresh_timer
                .schedule_refresh(&zone, start, soa, &loader.refresh_monitor);
        } else {
            state
                .loader
                .refresh_timer
                .schedule_retry(&zone, start, soa, &loader.refresh_monitor);
        }

        // Process the result of the reload.
        match result {
            Ok(None) => {
                // Nothing to do.
            }

            Ok(Some(serial)) => {
                // TODO: Inform the central command.
                let _ = serial;

                // Update the old-base contents.
                let latest = state.contents.as_ref().unwrap().latest.clone();
                let zone_copy = zone.clone();
                tokio::task::spawn(
                    async move { latest.write_into_zonetree(&zone_copy.loaded).await },
                );
            }

            Err(error) => {
                // TODO: Log the error?
                let _ = error;
            }
        }

        // Update the state of ongoing refreshes.
        let id = tokio::task::id();
        let enqueued = match state.loader.refreshes.take() {
            Some(Refreshes {
                ongoing: OngoingRefresh::Refresh { start: _, handle },
                enqueued,
            }) if handle.id() == id => enqueued,
            refreshes => panic!("ongoing refresh ({id:?}) is unregistered (state: {refreshes:?})"),
        };

        // Start the next enqueued refresh.
        if let Some(refresh) = enqueued {
            Self::start(state, zone.clone(), refresh, loader);
        }
    }

    /// Reload this zone.
    async fn reload(zone: Arc<Zone>, start: Instant, source: Source, loader: Arc<Loader>) {
        // Perform the source-specific reload into the zone contents.
        let (result, lock) = loader::reload(&zone, &source).await;

        // Try to re-use a recent lock from the reload.
        let mut lock = lock.unwrap_or_else(|| zone.data.lock().unwrap());
        let state = &mut *lock;

        // Update the zone refresh timers.
        let soa = state.contents.as_ref().map(|contents| &contents.latest.soa);
        if result.is_ok() {
            state
                .loader
                .refresh_timer
                .schedule_refresh(&zone, start, soa, &loader.refresh_monitor);
        } else {
            state
                .loader
                .refresh_timer
                .schedule_retry(&zone, start, soa, &loader.refresh_monitor);
        }

        // Process the result of the reload.
        match result {
            Ok(None) => {
                // Nothing to do.
            }

            Ok(Some(serial)) => {
                // TODO: Inform the central command.
                let _ = serial;

                // Update the old-base contents.
                let latest = state.contents.as_ref().unwrap().latest.clone();
                let zone_copy = zone.clone();
                tokio::task::spawn(
                    async move { latest.write_into_zonetree(&zone_copy.loaded).await },
                );
            }

            Err(error) => {
                // TODO: Log the error?
                let _ = error;
            }
        }

        // Update the state of ongoing refreshes.
        let id = tokio::task::id();
        let enqueued = match state.loader.refreshes.take() {
            Some(Refreshes {
                ongoing: OngoingRefresh::Reload { start: _, handle },
                enqueued,
            }) if handle.id() == id => enqueued,
            refreshes => panic!("ongoing reload ({id:?}) is unregistered (state: {refreshes:?})"),
        };

        // Start the next enqueued refresh.
        if let Some(refresh) = enqueued {
            Self::start(state, zone.clone(), refresh, loader);
        }
    }
}

//----------- Source -----------------------------------------------------------

/// The source of a zone.
//
// TODO: Support multiple sources for a zone?
#[derive(Clone, Debug, Default)]
pub enum Source {
    /// The lack of a source.
    ///
    /// The zone will not be loaded from any external source.  This is the
    /// default state for new zones.
    //
    // TODO: When DNS UPDATE messages are supported, the zone contents can be
    //   changed, making this a valid way to host a zone.
    #[default]
    None,

    /// A zonefile on disk.
    ///
    /// The specified path should point to a regular file (possibly through
    /// symlinks, as per OS limitations) containing the contents of the zone in
    /// the conventional "DNS zonefile" format.
    ///
    /// In addition to the default zone refresh triggers, the zonefile will also
    /// be monitored for changes (through OS-specific mechanisms), and will be
    /// refreshed when a change is detected.
    Zonefile {
        /// The path to the zonefile.
        path: Box<Utf8Path>,
    },

    /// A DNS server.
    ///
    /// The specified server will be queried for the contents of the zone using
    /// incremental and authoritative zone transfers (IXFRs and AXFRs).
    Server {
        /// The address of the server.
        addr: DnsServerAddr,
    },
}

//----------- RefreshTimerState ------------------------------------------------

/// State for the refresh timer of a zone.
#[derive(Debug, Default)]
pub enum RefreshTimerState {
    /// The refresh timer is disabled.
    ///
    /// The zone will not be refreshed automatically.  This is the default state
    /// for new zones, and is used when a local copy of the zone is unavailable.
    #[default]
    Disabled,

    /// Following up a previous successful refresh.
    ///
    /// The zone was recently refreshed successfully.  A new refresh will be
    /// enqueued following the SOA REFRESH timer.
    Refresh {
        /// When the previous (successful) refresh started.
        previous: Instant,

        /// The scheduled time for the next refresh.
        ///
        /// This is equal to `previous + soa.refresh`.  If the SOA record
        /// changes (e.g. due to a new version of the zone being loaded), this
        /// is recomputed, and the refresh is rescheduled accordingly.
        scheduled: Instant,
    },

    /// Following up a previous failing refresh.
    ///
    /// A previous refresh of the zone failed.  A new refresh will be enqueued
    /// following the SOA RETRY timer.
    Retry {
        /// When the previous (failing) refresh started.
        previous: Instant,

        /// The scheduled time for the next refresh.
        ///
        /// This is equal to `previous + soa.retry`.  If the SOA record changes
        /// (e.g. due to a new version of the zone being loaded), this is
        /// recomputed, and the refresh is rescheduled accordingly.
        scheduled: Instant,
    },
}

impl RefreshTimerState {
    /// The currently scheduled refresh time, if any.
    pub const fn scheduled_time(&self) -> Option<Instant> {
        match *self {
            Self::Disabled => None,
            Self::Refresh { scheduled, .. } => Some(scheduled),
            Self::Retry { scheduled, .. } => Some(scheduled),
        }
    }

    /// Disable zone refreshing.
    ///
    /// This is called when the zone contents are wiped or the zone source is
    /// removed.
    pub fn disable(&mut self, zone: &Arc<Zone>, monitor: &RefreshMonitor) {
        monitor.update(zone, self.scheduled_time(), None);
        *self = Self::Disabled;
    }

    /// Schedule a refresh.
    ///
    /// This is called when a previous refresh completes successfully.
    pub fn schedule_refresh(
        &mut self,
        zone: &Arc<Zone>,
        previous: Instant,
        soa: Option<&SoaRecord>,
        monitor: &RefreshMonitor,
    ) {
        // If a SOA record is unavailable, don't schedule anything.
        let Some(soa) = soa else {
            monitor.update(zone, self.scheduled_time(), None);
            *self = Self::Disabled;
            return;
        };

        let refresh = Duration::from_secs(soa.rdata.refresh.get().into());
        let scheduled = previous + refresh;
        monitor.update(zone, self.scheduled_time(), Some(scheduled));
        *self = Self::Refresh {
            previous,
            scheduled,
        };
    }

    /// Schedule a retry.
    ///
    /// This is called when a previous refresh fails.
    pub fn schedule_retry(
        &mut self,
        zone: &Arc<Zone>,
        previous: Instant,
        soa: Option<&SoaRecord>,
        monitor: &RefreshMonitor,
    ) {
        // If a SOA record is unavailable, don't schedule anything.
        let Some(soa) = soa else {
            monitor.update(zone, self.scheduled_time(), None);
            *self = Self::Disabled;
            return;
        };

        let retry = Duration::from_secs(soa.rdata.retry.get().into());
        let scheduled = previous + retry;
        monitor.update(zone, self.scheduled_time(), Some(scheduled));
        *self = Self::Retry {
            previous,
            scheduled,
        };
    }
}

//----------- Refreshes --------------------------------------------------------

/// Ongoing and enqueued refreshes of a zone.
#[derive(Debug)]
pub struct Refreshes {
    /// A handle to an ongoing refresh.
    pub ongoing: OngoingRefresh,

    /// An enqueued refresh.
    ///
    /// If multiple refreshes/reloads are enqueued, they are merged together.
    pub enqueued: Option<EnqueuedRefresh>,
}

impl Refreshes {
    /// Construct a new [`Refreshes`].
    pub fn new(ongoing: OngoingRefresh) -> Self {
        Self {
            ongoing,
            enqueued: None,
        }
    }

    /// Enqueue a refresh/reload.
    ///
    /// If one is already enqueued, the two will be merged.
    pub fn enqueue(&mut self, refresh: EnqueuedRefresh) {
        self.enqueued = self.enqueued.take().max(Some(refresh));
    }
}

//----------- OngoingRefresh ---------------------------------------------------

/// An ongoing refresh or reload of a zone.
#[derive(Debug)]
pub enum OngoingRefresh {
    /// An ongoing refresh.
    Refresh {
        /// When the refresh started.
        start: Instant,

        /// A handle to the refresh.
        handle: tokio::task::JoinHandle<()>,
    },

    /// An ongoing reload.
    Reload {
        /// When the reload started.
        start: Instant,

        /// A handle to the reload.
        handle: tokio::task::JoinHandle<()>,
    },
}

//----------- EnqueuedRefresh --------------------------------------------------

/// An enqueued refresh or reload of a zone.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EnqueuedRefresh {
    /// An enqueued refresh.
    Refresh,

    /// An enqueued reload.
    Reload,
}

//----------- DnsServerAddr ----------------------------------------------------

/// How to connect to a DNS server.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DnsServerAddr {
    /// The Internet address.
    pub ip: IpAddr,

    /// The TCP port number.
    pub tcp_port: u16,

    /// The UDP port number, if it's supported.
    pub udp_port: Option<u16>,
}
