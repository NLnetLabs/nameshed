use core::fmt::Debug;
use core::pin::Pin;
use core::sync::atomic::AtomicBool;
use core::task::{Context, Poll};

use std::boxed::Box;
use std::collections::HashMap;
use std::fmt::Display;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::FutureExt;
use serde::{Serialize, Serializer};
use tokio::time::{Instant, Sleep};

use core::time::Duration;
use domain::base::iana::Class;
use domain::base::{CanonicalOrd, Serial, Ttl};
use domain::tsig::{Algorithm, Key, KeyName};
use domain::zonetree::{StoredName, Zone};

//------------ Type Aliases --------------------------------------------------

/// A store of TSIG keys index by key name and algorithm.
#[allow(dead_code)]
pub type ZoneMaintainerKeyStore = HashMap<(KeyName, Algorithm), Key>;

//------------ ZoneId --------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct ZoneId {
    pub name: StoredName,
    pub class: Class,
}

impl From<&Zone> for ZoneId {
    fn from(zone: &Zone) -> Self {
        ZoneId {
            name: zone.apex_name().to_owned(),
            class: zone.class(),
        }
    }
}

impl From<Zone> for ZoneId {
    fn from(zone: Zone) -> Self {
        ZoneId {
            name: zone.apex_name().to_owned(),
            class: zone.class(),
        }
    }
}

//------------ SrcDstConfig --------------------------------------------------

/// A mapping of network source/destination to some config `T`.
///
/// Maps source addresses (a `SocketAddress` with port 0, i.e. just an
/// `IpAddr`, as we can't know in advance the port number a caller will use),
/// or destination addresses (a `SocketAddr` including port), to some user
/// provided data.
///
/// TODO: Change this to support net blocks as the source once PR 340 (which
/// extends COOKIE middleware to use net blocks) is resolved.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SrcDstConfig<T: Clone + Debug + Default> {
    entries: HashMap<SocketAddr, T>,
}

//------------ XfrStrategy ---------------------------------------------------

/// Which modes of XFR to support.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum XfrStrategy {
    /// Do not support XFR at all.
    #[default]
    None,
}

//------------ IxfrTransportStrategy -----------------------------------------

/// Which modes of transport to support.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum TransportStrategy {
    #[default]
    None,
}

//--- Display

impl Display for TransportStrategy {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TransportStrategy::None => f.write_str("None"),
        }
    }
}

//------------ CompatibilityMode ---------------------------------------------

/// https://datatracker.ietf.org/doc/html/rfc5936#section-7.1
/// 7.1.  Server
///   "An implementation of an AXFR server MAY permit configuring, on a per
///    AXFR client basis, the necessity to revert to a single resource record
///    per message; in that case, the default SHOULD be to use multiple
///    records per message."
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub enum CompatibilityMode {
    #[default]
    Default,
}

//------------ XfrConfig -----------------------------------------------------

#[derive(Clone, Debug, Default, Serialize)]
pub struct XfrConfig {
    pub strategy: XfrStrategy,
    pub ixfr_transport: TransportStrategy,
    pub compatibility_mode: CompatibilityMode,
}

//------------ NotifyConfig --------------------------------------------------

#[derive(Clone, Debug, Default, Serialize)]
pub struct NotifyConfig {}

//------------ Type Aliases --------------------------------------------------

pub type XfrSrcDstConfig = SrcDstConfig<XfrConfig>;
pub type NotifySrcDstConfig = SrcDstConfig<NotifyConfig>;

//------------ NotifyStrategy ------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub enum NotifyStrategy {
    #[default]
    NotifySourceFirstThenSequentialStoppingAtFirstNewerSerial,
}

//------------ ZoneType ------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize)]
pub struct ZoneConfig {
    pub multi_primary_xfr_strategy: NotifyStrategy,
    pub discover_notify_set: bool,
    pub provide_xfr_to: XfrSrcDstConfig,
    pub send_notify_to: NotifySrcDstConfig,
    pub allow_notify_from: NotifySrcDstConfig,
    pub request_xfr_from: XfrSrcDstConfig,
}

//------------ ZoneDiffKey ---------------------------------------------------

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct ZoneDiffKey {
    start_serial: Serial,
    end_serial: Serial,
}

impl Ord for ZoneDiffKey {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.start_serial.canonical_cmp(&other.start_serial)
    }
}

impl PartialOrd for ZoneDiffKey {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

//------------ ZoneStatus ----------------------------------------------------

#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize)]
pub enum ZoneRefreshStatus {
    /// Refreshing according to the SOA REFRESH interval.
    #[default]
    RefreshPending,
}

//--- Display

impl Display for ZoneRefreshStatus {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ZoneRefreshStatus::RefreshPending => f.write_str("refresh pending"),
        }
    }
}

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

pub fn serialize_opt_instant_as_duration_secs<S>(
    instant: &Option<Instant>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match instant {
        Some(v) => serialize_instant_as_duration_secs(v, serializer),
        None => serializer.serialize_str("null"),
    }
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

#[derive(Clone, Copy, Debug, Serialize)]
pub struct ZoneRefreshMetrics {
    #[serde(serialize_with = "serialize_instant_as_duration_secs")]
    pub zone_created_at: Instant,

    /// None means never checked
    #[serde(serialize_with = "serialize_opt_instant_as_duration_secs")]
    pub last_refresh_phase_started_at: Option<Instant>,

    /// None means never checked
    #[serde(serialize_with = "serialize_opt_instant_as_duration_secs")]
    pub last_refresh_attempted_at: Option<Instant>,

    /// None means never checked
    #[serde(serialize_with = "serialize_opt_instant_as_duration_secs")]
    pub last_soa_serial_check_succeeded_at: Option<Instant>,

    /// None means never checked
    ///
    /// The SOA SERIAL received for the last successful SOA query sent to a
    /// primary for this zone.
    pub last_soa_serial_check_serial: Option<Serial>,

    /// None means never refreshed
    #[serde(serialize_with = "serialize_opt_instant_as_duration_secs")]
    pub last_refreshed_at: Option<Instant>,

    /// None means never refreshed
    ///
    /// The SOA SERIAL of the last commit made to this zone.
    pub last_refresh_succeeded_serial: Option<Serial>,
}

impl Default for ZoneRefreshMetrics {
    fn default() -> Self {
        Self {
            zone_created_at: Instant::now(),
            last_refresh_phase_started_at: Default::default(),
            last_refresh_attempted_at: Default::default(),
            last_soa_serial_check_succeeded_at: Default::default(),
            last_soa_serial_check_serial: Default::default(),
            last_refreshed_at: Default::default(),
            last_refresh_succeeded_serial: Default::default(),
        }
    }
}

//------------ ZoneRefreshState ----------------------------------------------

#[derive(Clone, Copy, Debug, Serialize)]
pub struct ZoneRefreshState {
    /// SOA REFRESH
    refresh: Ttl,

    /// SOA RETRY
    retry: Ttl,

    /// SOA EXPIRE
    expire: Ttl,

    /// Refresh status
    status: ZoneRefreshStatus,

    /// Refresh metrics
    metrics: ZoneRefreshMetrics,
}

impl Default for ZoneRefreshState {
    fn default() -> Self {
        // These values affect how hard and fast we try to provision a
        // secondary zone on startup.
        // TODO: These values should be configurable.
        Self {
            refresh: Ttl::ZERO,
            retry: Ttl::from_mins(5),
            expire: Ttl::from_hours(1),
            status: Default::default(),
            metrics: Default::default(),
        }
    }
}

//------------ ZoneRefreshInstant --------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct ZoneRefreshInstant {
    pub cause: ZoneRefreshCause,
    pub zone_id: ZoneId,
    #[serde(serialize_with = "serialize_instant_as_duration_secs")]
    pub end_instant: Instant,
}

//------------ ZoneRefreshCause ----------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub enum ZoneRefreshCause {}

//------------ ZoneRefreshTimer ----------------------------------------------

pub(super) struct ZoneRefreshTimer {
    pub refresh_instant: ZoneRefreshInstant,
    pub sleep_fut: Pin<Box<Sleep>>,
}

impl Future for ZoneRefreshTimer {
    type Output = ZoneRefreshInstant;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.sleep_fut.poll_unpin(cx) {
            Poll::Ready(()) => Poll::Ready(self.refresh_instant.clone()),
            Poll::Pending => Poll::Pending,
        }
    }
}

//------------ ZoneInfo ------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct ZoneInfo {
    pub(super) _catalog_member_id: Option<StoredName>,
    pub(super) config: ZoneConfig,
    pub(super) expired: Arc<AtomicBool>,
}
