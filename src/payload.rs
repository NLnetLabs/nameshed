//------------ Update --------------------------------------------------------

use domain::base::Serial;
use domain::zonetree::StoredName;

#[derive(Clone, Debug)]
pub enum Update {
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
}
