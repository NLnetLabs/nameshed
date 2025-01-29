//------------ Update --------------------------------------------------------

use domain::zonetree::StoredName;
use domain::base::Serial;

#[derive(Clone, Debug)]
pub enum Update {
    // Define events here.
    ZoneUpdatedEvent {
        zone_name: StoredName,
        zone_serial: Serial,
    },
    ZoneApprovedEvent {
        zone_name: StoredName,
        zone_serial: Serial,
    },
}
