use std::net::IpAddr;

use domain::base::Serial;
use domain::zonetree::StoredName;

//------------ Update --------------------------------------------------------

#[allow(clippy::enum_variant_names)]
#[derive(Clone, Debug)]
pub enum Update {
    /// A request to refresh a zone.
    ///
    /// This is sent by the publication server when it receives an appropriate
    /// NOTIFY message.
    RefreshZone {
        /// The name of the zone to refresh.
        zone_name: StoredName,

        /// The source address of the NOTIFY message.
        source: Option<IpAddr>,

        /// The expected new SOA serial for the zone.
        ///
        /// If this is set, and the zone's SOA serial is greater than or equal
        /// to this value, the refresh can be ignored.
        serial: Option<Serial>,
    },

    UnsignedZoneUpdatedEvent {
        zone_name: StoredName,
        zone_serial: Serial,
    },
    UnsignedZoneApprovedEvent {
        zone_name: StoredName,
        zone_serial: Serial,
    },
    ZoneSignedEvent {
        zone_name: StoredName,
        zone_serial: Serial,
    },

    SignedZoneApprovedEvent {
        zone_name: StoredName,
        zone_serial: Serial,
    },

    ResignZoneEvent {
        zone_name: StoredName,
    },
}
