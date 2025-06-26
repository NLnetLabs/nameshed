//! Communication between components.
//!
//! The main purpose of communication is for a unit is to announce updates to
//! its data set and operational state to all other components that are
//! interested. It also takes care of managing these communication lines.
//!
//! There are three main types here: Each unit has a single [`Gate`] to
//! which it hands its updates. The opposite end is called a [`Link`] and
//! is held by any interested component. A [`GateAgent`] is a reference to a
//! gate that can be used to create new links.
//!
//! The type [`GateMetrics`] can be used by units to provide some obvious
//! metrics such as the number of payload units in the data set or the time
//! of last update based on the updates sent to the gate.
//!
//! To connect a Link to a Gate a Subscribe command is sent via message queue
//! from the Link to the Gate using the Link `connect()` fn. Data updates are
//! broadcast from a Gate to each of its connected Links by invoking the Gate
//! `update_data()` fn. Links receive data updates via message queue. A
//! specialized type of Link called a DirectLink receives updates by direct
//! function invocation instead of by message queue, to a function registered
//! using the DirectLink variant of the `connect()` fn. Direct function
//! invocation has lower overhead and is faster, but requires the sending gate
//! to wait for the invoked function to complete before the next update can be
//! sent, i.e. the speed at which the component owning the Gate is able to
//! broadcast updates is limited by the speed of any functions registered by
//! connected DirectLinks. Conversely, a message queue allows linked
//! components to operate at different speeds which may be useful for handling
//! bursts of data, but if the publishing component is consistently faster
//! than the receiving component the message queue will become full and block
//! further updates until space becomes available again in the receive queue.
//! A Link serializes the incoming updates whereas a DirectLink can receive
//! multiple updates in parallel at the same time.
//!
//! Links receive data updates as long as they are connected to a Gate and
//! have not been suspended. Normally the Gate `get_gate_status()` fn will
//! report the Gate as Active, but if all of the Links connected to a Gate are
//! suspended the Gate is said to be Dormant, otherwise it is said to be
//! Active. A Link can be suspended via the Link `suspend()` fn. To receive
//! updates that were sent via message queue the owning component must use the
//! Link `query()` fn. Calling `query()` will unsuspend a suspended Link.
//!
//! It is not possible to publish concurrently from multiple threads via the
//! same Gate. To support components that receive data from multiple threads
//! at once without requiring them to lock the Gate in order to publish to it
//! one at a time a component can invoke the Gate `clone()` fn to give each
//! publishing thread a clone of the original Gate. However, only a subset of
//! the operations that can be performed on a Gate can be performed on a clone
//! of the Gate. If a Gate is terminated or dropped any clones of the Gate
//! will also be terminated and stop accepting updates.
//!
//! Gates do not automatically respond to commands. To process commands the
//! owning component must use the Gate `process()`, `process_until()` or
//! `wait()` functions. In addition to the basic subscribe and unsubscribe
//! commands, gates also support a few special commands. The Reconfigure
//! command is used to give feedback to the component owning a gate when the
//! configuration of the gate should be changed. The ReportLinks command is
//! used to query components for the set of Links in use by the Gate owning
//! component to receive incoming updates. Gate owning components can be
//! instructed to shutdown via the Terminate command, and Gates owning
//! components can be triggered via the Trigger command by a downstream link
//! to cause the upstream component to do something, e.g. perform a lookup
//! or calculation and pass the result back down through the Gate as a
//! QueryResult update. Additional commands are used internally to keep Gate
//! clones configuration in sync with that of the original Gate.

use domain::base::Serial;
use domain::zonetree::StoredName;
use std::fmt::{self, Debug};

//------------ GraphMetrics --------------------------------------------------
pub trait GraphStatus: Send + Sync {
    fn status_text(&self) -> String;

    fn okay(&self) -> Option<bool> {
        None
    }
}

//------------ UnitStatus ----------------------------------------------------

/// The operational status of a unit.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UnitStatus {
    /// The unit is ready to produce data updates.
    ///
    /// Note that this status does not necessarily mean that the unit is
    /// actually producing updates, only that it could. That is, if a unit’s
    /// gate is dormant and the unit ceases operation because nobody cares,
    /// it is still in healthy status.
    #[default]
    Healthy,

    /// The unit had to temporarily suspend operation.
    ///
    /// If it sets this status, the unit will try to become healthy again
    /// later. The status is typically used if a server has become
    /// unavailable.
    Stalled,

    /// The unit had to permanently suspend operation.
    ///
    /// This status indicates that the unit will not become healthy ever
    /// again. Links to the unit can safely be dropped.
    Gone,
}

impl fmt::Display for UnitStatus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match *self {
            UnitStatus::Healthy => "healthy",
            UnitStatus::Stalled => "stalled",
            UnitStatus::Gone => "gone",
        })
    }
}

//------------ Terminated ----------------------------------------------------

/// An error signalling that a unit has been terminated.
///
/// In response to this error, a unit’s run function should return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Terminated;

#[allow(clippy::enum_variant_names)]
#[derive(Clone, Debug)]
pub enum ApplicationCommand {
    Terminate,
    SeekApprovalForUnsignedZone {
        zone_name: StoredName,
        zone_serial: Serial,
    },
    SignZone {
        zone_name: StoredName,
        zone_serial: Serial,
    },
    SeekApprovalForSignedZone {
        zone_name: StoredName,
        zone_serial: Serial,
    },
    PublishSignedZone {
        zone_name: StoredName,
        zone_serial: Serial,
    },
}
