//! Zone-specific loader state.

use std::{net::IpAddr, sync::Arc};

use camino::Utf8Path;

use super::{Zone, ZoneState};

//----------- LoaderState ------------------------------------------------------

/// State for loading new versions of a zone.
#[derive(Debug, Default)]
pub struct LoaderState {
    /// The source of the zone.
    pub source: Source,

    /// Ongoing and enqueued reloads of the zone.
    pub reloads: Option<Reloads>,
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

    /// Enqueue a reload of this zone.
    ///
    /// If the zone is not being reloaded already, a new reload will be
    /// initiated.  Otherwise, a reload will be enqueued; if one is enqueued
    /// already, it will be merged.
    ///
    /// # Standards
    ///
    /// Complies with [RFC 1996, section 4.4], when this is used to enqueue a
    /// reload in response to a `QTYPE=SOA` NOTIFY message.
    ///
    /// > 4.4. A slave which receives a valid NOTIFY should defer action on any
    /// > subsequent NOTIFY with the same \<QNAME,QCLASS,QTYPE\> until it has
    /// > completed the transaction begun by the first NOTIFY.  This duplicate
    /// > rejection is necessary to avoid having multiple notifications lead to
    /// > pummeling the master server.
    ///
    /// [RFC 1996, section 4.4]: https://datatracker.ietf.org/doc/html/rfc1996#section-4
    pub fn enqueue_reload(state: &mut ZoneState, zone: Arc<Zone>) {
        // TODO: Log

        // Determine whether a reload is ongoing.
        match &mut state.loader.reloads {
            Some(reloads) => {
                // There is an ongoing reload.  Enqueue a new one, which will
                // start when the ongoing one finishes.  If a reload is already
                // enqueued, the two will be merged.
                reloads.enqueued = true;
            }

            None => {
                // There is no ongoing reload.  Initiate one immediately.

                let _ = zone;
                todo!()
            }
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
    /// The zone will not be loaded from any external source.  Nameshed will
    /// consider itself authoritative for this zone, including for any versions
    /// of the zone that have been loaded already (through other sources).
    ///
    /// This is the default state for new zones.
    //
    // TODO: When DNS UPDATE messages are supported, the zone contents can be
    //   changed, making this a valid way to host a zone authoritatively.
    #[default]
    None,

    /// A zonefile on disk.
    ///
    /// The specified path should point to a regular file (possibly through
    /// symlinks, as per OS limitations) containing the contents of the zone in
    /// the conventional "DNS zonefile" format.
    ///
    /// In addition to the default zone reload triggers, the zonefile will also
    /// be monitored for changes (through OS-specific mechanisms), and will be
    /// reloaded when a change is detected.
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

//----------- Reloads ----------------------------------------------------------

/// Ongoing and enqueued reloads of a zone.
#[derive(Debug)]
pub struct Reloads {
    /// A handle to an ongoing reload.
    ///
    /// The reload is processed in a new Tokio task, whose handle is attached
    /// here.  This can be used to detect completion and retrieve the result.
    //
    // TODO: Consider how to respond to a reload error.  Perhaps that should
    //   simply log an error and update the refresh timers.
    pub ongoing: tokio::task::JoinHandle<()>,

    /// Whether a reload is enqueued.
    ///
    /// If this is `true`, a new reload will be started when the current one
    /// finishes.  Note that at most one reload can be enqueued; if a new reload
    /// is requested, it will be merged with this one.
    pub enqueued: bool,
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
