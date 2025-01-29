//------------ Update --------------------------------------------------------

use domain::zonetree::StoredName;

#[derive(Clone, Debug)]
pub enum Update {
    // Define events here.
    ZoneUpdatedEvent(StoredName),
    ZoneApprovedEvent(StoredName),
}
