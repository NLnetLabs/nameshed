//! Zone-specific loader state.

use std::{net::IpAddr, sync::Arc, time::Instant};

use camino::Utf8Path;

use crate::loader;

use super::{Zone, ZoneState};

//----------- LoaderState ------------------------------------------------------

/// State for loading new versions of a zone.
#[derive(Debug, Default)]
pub struct LoaderState {
    /// The source of the zone.
    pub source: Source,

    /// Ongoing and enqueued refreshes of the zone.
    pub refreshes: Option<Refreshes>,
    //
    // TODO:
    // - File monitoring state
    // - Refresh monitoring state
}

impl LoaderState {
    /// Set the source of this zone.
    pub fn set_source(state: &mut ZoneState, zone: Arc<Zone>, source: Source) {
        // TODO: Log
        //
        // TODO: Should we enqueue a reload now, or do we leave that to the
        //   caller?  Issuing reloads during the initialization of a zone might
        //   not be the best idea.

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
    pub fn enqueue_refresh(state: &mut ZoneState, zone: Arc<Zone>, reload: bool) {
        // TODO: Log

        // Determine whether a refresh is ongoing.
        if let Some(refreshes) = &mut state.loader.refreshes {
            // There is an ongoing refresh.  Enqueue a new one, which will
            // start when the ongoing one finishes.  If a refresh is already
            // enqueued, the two will be merged.
            refreshes.enqueue(if reload {
                EnqueuedRefresh::Reload
            } else {
                EnqueuedRefresh::Refresh
            });
            return;
        }

        // There is no ongoing refresh.  Initiate one immediately.
        let start = Instant::now();
        let source = state.loader.source.clone();
        let ongoing = if reload {
            let handle = tokio::task::spawn(async move {
                // Reload the zone.
                let serial = match loader::reload(&zone, &source).await {
                    Ok(serial) => serial,
                    Err(error) => {
                        // Update the zone refresh timers.
                        let _ = error;
                        todo!()
                    }
                };

                if let Some(serial) = serial {
                    // TODO: Inform the central command.
                    let _ = serial;
                }
                // TODO: Update the zone refresh timers.
            });
            OngoingRefresh::Reload { start, handle }
        } else {
            let latest = state
                .contents
                .as_ref()
                .map(|contents| contents.latest.clone());

            let handle = tokio::task::spawn(async move {
                // Refresh the zone.
                let serial = match loader::refresh(&zone, &source, latest).await {
                    Ok(serial) => serial,
                    Err(error) => {
                        // Update the zone refresh timers.
                        let _ = error;
                        todo!()
                    }
                };

                if let Some(serial) = serial {
                    // TODO: Inform the central command.
                    let _ = serial;
                }
                // TODO: Update the zone refresh timers.
            });
            OngoingRefresh::Refresh { start, handle }
        };
        state.loader.refreshes = Some(Refreshes::new(ongoing));
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
