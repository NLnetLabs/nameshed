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

use crate::common::frim::FrimMap;
use crate::metrics;
use crate::metrics::{Metric, MetricType, MetricUnit};
use crate::payload::Update;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crossbeam_utils::atomic::AtomicCell;
use futures::future::{select, Either, Future};
use futures::pin_mut;
use log::{log_enabled, trace, Level};

use domain::base::Serial;
use domain::zonetree::StoredName;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use std::{
    any::Any,
    fmt::{self, Debug, Display},
};
use tokio::sync::{mpsc, RwLock};
use tokio::time::{timeout_at, Instant};
use uuid::Uuid;

#[async_trait]
pub trait DirectUpdate {
    async fn direct_update(&self, update: Update);
}

pub trait AnyDirectUpdate: Any + Debug + Send + Sync + DirectUpdate {}

//------------ Configuration -------------------------------------------------

/// The queue length of an update channel.
pub const DEF_UPDATE_QUEUE_LEN: usize = 8;

/// The queue length of a command channel.
const COMMAND_QUEUE_LEN: usize = 16;

//------------ Gate ----------------------------------------------------------

#[derive(Debug)]
pub struct NormalGateState {
    /// Sender to our command receiver. Cloned when creating a clone of this
    /// Gate so that the cloned Gate can notify us when it is dropped. Only
    /// root Gates have this set, not their clones (if any).
    command_sender: mpsc::Sender<GateCommand>,
}

#[derive(Debug)]
pub enum GateState {
    Normal(NormalGateState),
}

/// A communication gate representing the source of data.
///
/// Each unit receives exactly one gate. Whenever it has new data or its
/// status changes, it sends these to (through?) the gate which takes care
/// of distributing the information to whomever is interested.
///
/// A gate may be active or dormant. It is active if there is at least one
/// party interested in receiving data updates. Otherwise it is dormant.
/// Obviously, there is no need for a unit with a dormant gate to produce
/// any updates. Units are, in fact, encouraged to suspend their operation
/// until their gate becomes active again.
///
/// In order for the gate to maintain its own state, the unit needs to
/// regularly run the [`process`](Self::process) method. In return,
/// the unit will receive an update to the gate’s state as soon as it
/// becomes available.
///
/// Sending of updates happens via the [`update_data`](Self::update_data)
/// method.
#[derive(Debug)]
pub struct Gate {
    id: Arc<Mutex<Uuid>>,

    name: Arc<String>,

    /// Receiver for commands sent in by the links.
    commands: Arc<RwLock<mpsc::Receiver<GateCommand>>>,

    /// Senders to all links.
    updates: Arc<FrimMap<Uuid, UpdateSender>>,

    /// The maximum number of updates to queue per link.
    queue_size: usize,

    /// Suspended senders.
    suspended: Arc<FrimMap<Uuid, UpdateSender>>,

    /// The gate metrics.
    metrics: Arc<GateMetrics>,

    /// Gate type dependent state.
    state: GateState,
}

// On drop, notify the parent of a cloned gate that this clone is detaching
// itself so that the parent Gate remove the corresponding entry from its
// clone senders collection, otherwise clones "leak" memory in the parent
// because a reference to them is never cleaned up.
impl Drop for Gate {
    fn drop(&mut self) {
        if log_enabled!(Level::Trace) {
            trace!("Gate[{} ({})]: Drop", self.name, self.id());
        }
    }
}

impl Default for Gate {
    fn default() -> Self {
        Self::new(DEF_UPDATE_QUEUE_LEN).0
    }
}

impl Gate {
    /// Creates a new gate.
    ///
    /// The function returns a gate and a gate agent that allows creating new
    /// links. Typically, you would pass the gate to a subsequently created
    /// unit and keep the agent around for future use.
    pub fn new(queue_size: usize) -> (Gate, GateAgent) {
        let (tx, rx) = mpsc::channel(COMMAND_QUEUE_LEN);
        let gate = Gate {
            id: Arc::new(Mutex::new(Uuid::new_v4())),
            name: Arc::default(),
            commands: Arc::new(RwLock::new(rx)),
            updates: Default::default(),
            queue_size,
            suspended: Default::default(),
            metrics: Default::default(),
            state: GateState::Normal(NormalGateState {
                command_sender: tx.clone(),
            }),
        };
        let agent = GateAgent {
            id: gate.id.clone(),
            commands: tx,
        };
        if log_enabled!(Level::Trace) {
            trace!("Gate[{} ({})]: New gate created", gate.name, gate.id());
        }
        (gate, agent)
    }

    /// Take the key internals of a Gate to use elsewhere.
    ///
    /// Can't be done manually via destructuring due to the existence of the
    /// Drop impl for Gate.
    ///
    /// For internal use only, hence not public.
    fn take(self) -> (mpsc::Receiver<GateCommand>, FrimMap<Uuid, UpdateSender>) {
        let commands = self.commands.clone();
        let updates = self.updates.clone();
        drop(self);
        let commands = Arc::try_unwrap(commands).unwrap().into_inner();
        let updates = Arc::try_unwrap(updates).unwrap();
        (commands, updates)
    }

    pub fn id(&self) -> Uuid {
        *self.id.lock().unwrap()
    }

    /// Returns a shareable reference to the gate metrics.
    ///
    /// Metrics are shared between a gate and its clones.
    pub fn metrics(&self) -> Arc<GateMetrics> {
        self.metrics.clone()
    }

    /// Runs the gate’s internal machine.
    ///
    /// This method returns a future that runs the gate’s internal machine.
    /// It resolves once the gate’s status changes. It can be dropped at any
    /// time. In this case, the gate will pick up where it left off when the
    /// method is called again.
    ///
    /// The method will resolve into an error if the unit should terminate.
    /// This is the case if all links and gate agents referring to the gate
    /// have been dropped.
    ///
    /// # Panics
    ///
    /// Panics if a cloned gate receives `GateCommand::Subscribe` or a
    /// non-cloned gate receives `GateCommand::FollowSubscribe`.
    pub async fn process(&self) -> Result<GateStatus, Terminated> {
        let status = self.get_gate_status();
        let command = {
            let mut lock = self.commands.write().await;
            match lock.recv().await {
                Some(command) => command,
                None => {
                    if log_enabled!(Level::Trace) {
                        trace!(
                            "Gate[{} ({})]: Command channel has been closed",
                            self.name,
                            self.id()
                        );
                    }
                    return Err(Terminated);
                }
            }
        };

        if log_enabled!(Level::Trace) {
            trace!(
                "Gate[{} ({})]: Received command '{}'",
                self.name,
                self.id(),
                command
            );
        }

        match command {
            GateCommand::ApplicationCommand { data } => {
                Ok(GateStatus::ApplicationCommand { cmd: data })
            }

            GateCommand::Terminate => Err(Terminated),
        }
    }

    /// Runs the gate’s internal machine until a future resolves.
    ///
    /// Ignores any gate status changes.
    ///
    /// # Panics
    ///
    /// See [process()](Self::process).
    pub async fn process_until<Fut: Future>(&self, fut: Fut) -> Result<Fut::Output, Terminated> {
        pin_mut!(fut);

        loop {
            let process = self.process();
            pin_mut!(process);
            match select(process, fut).await {
                Either::Left((Err(_), _)) => return Err(Terminated),
                Either::Left((Ok(_), next_fut)) => {
                    fut = next_fut;
                }
                Either::Right((res, _)) => return Ok(res),
            }
        }
    }

    /// Runs the gate's internal machine for a period of time.
    ///
    /// # Panics
    ///
    /// See [process()](Self::process).
    pub async fn wait(&self, secs: u64) -> Result<(), Terminated> {
        let end = Instant::now() + Duration::from_secs(secs);

        while end > Instant::now() {
            match timeout_at(end, self.process()).await {
                Ok(Ok(_status)) => {
                    // Wait interrupted by internal gate status change, keep
                    // waiting
                }
                Ok(Err(Terminated)) => {
                    // Wait interrupted by gate termination, abort
                    return Err(Terminated);
                }
                Err(_) => {
                    // Wait completed
                    return Ok(());
                }
            }
        }

        // Wait end time passed
        Ok(())
    }

    /// Returns the current gate status.
    pub fn get_gate_status(&self) -> GateStatus {
        if self.suspended.len() == self.updates.len() {
            GateStatus::Dormant
        } else {
            GateStatus::Active
        }
    }

    pub(crate) fn set_name(&mut self, name: &str) {
        self.name = Arc::new(name.to_string());
    }

    pub fn name(&self) -> Arc<String> {
        self.name.clone()
    }
}

//------------ GateAgent -----------------------------------------------------

/// A representative of a gate allowing creation of new links for it.
///
/// The agent can be cloned and passed along. The method
/// [`create_link`](Self::create_link) can be used to create a new link.
///
/// Yes, the name is a bit of a mixed analogy.
#[derive(Clone, Debug)]
pub struct GateAgent {
    id: Arc<Mutex<Uuid>>,
    commands: mpsc::Sender<GateCommand>,
}

impl GateAgent {
    pub fn id(&self) -> Uuid {
        *self.id.lock().unwrap()
    }

    pub async fn terminate(&self) {
        let _ = self.commands.send(GateCommand::Terminate).await;
    }

    pub fn is_terminated(&self) -> bool {
        self.commands.is_closed()
    }

    pub async fn send_application_command(&self, data: ApplicationCommand) -> Result<(), String> {
        self.commands
            .send(GateCommand::ApplicationCommand { data })
            .await
            .map_err(|err| format!("{}", err))
    }
}

//------------ GraphMetrics --------------------------------------------------
pub trait GraphStatus: Send + Sync {
    fn status_text(&self) -> String;

    fn okay(&self) -> Option<bool> {
        None
    }
}

//------------ GateMetrics ---------------------------------------------------

/// Metrics about the updates distributed via the gate.
///
/// This type is a [`metrics::Source`](crate::metrics::Source) that provides a
/// number of metrics for a unit that can be derived from the updates sent by
/// the unit and thus are common to all units.
///
/// Gates provide access to values of this type via the [`Gate::metrics`]
/// method. When stored behind an arc t can be kept and passed around freely.
#[derive(Debug, Default)]
pub struct GateMetrics {
    /// The number of payload items in the last update.
    pub update_set_size: AtomicUsize,

    /// The date and time of the last update.
    ///
    /// If there has never been an update, this will be `None`.
    pub update: AtomicCell<Option<DateTime<Utc>>>,

    /// The number of updates sent through the gate
    pub num_updates: AtomicUsize,

    /// The number of updates that could not be sent through the gate
    pub num_dropped_updates: AtomicUsize,
}

impl GraphStatus for GateMetrics {
    fn status_text(&self) -> String {
        format!("out: {}", self.num_updates.load(SeqCst))
    }
}

impl GateMetrics {
    /// Updates the metrics to match the given update.
    fn update(
        &self,
        _update: &Update,
        _senders: Arc<FrimMap<Uuid, UpdateSender>>,
        sent_at_least_once: bool,
    ) {
        self.num_updates.fetch_add(1, SeqCst);
        if !sent_at_least_once {
            self.num_dropped_updates.fetch_add(1, SeqCst);
        }
        // if let Event::Bulk(update) = update {
        //     self.update_set_size.store(update.len(), SeqCst);
        // }
        self.update.store(Some(Utc::now()));
    }
}

impl GateMetrics {
    const NUM_UPDATES_METRIC: Metric = Metric::new(
        "num_updates",
        "the number of updates sent through the gate",
        MetricType::Counter,
        MetricUnit::Total,
    );
    const NUM_DROPPED_UPDATES_METRIC: Metric = Metric::new(
        "num_dropped_updates",
        "the number of updates that could not be sent through the gate",
        MetricType::Counter,
        MetricUnit::Total,
    );
    const UPDATE_SET_SIZE_METRIC: Metric = Metric::new(
        "update_set_size",
        "the number of set items in the last update",
        MetricType::Gauge,
        MetricUnit::Total,
    );
    const UPDATE_WHEN_METRIC: Metric = Metric::new(
        "last_update",
        "the date and time of the last update",
        MetricType::Text,
        MetricUnit::Info,
    );
    const UPDATE_AGO_METRIC: Metric = Metric::new(
        "since_last_update",
        "the number of seconds since the last update",
        MetricType::Gauge,
        MetricUnit::Second,
    );
}

impl metrics::Source for GateMetrics {
    /// Appends the current gate metrics to a target.
    ///
    /// The name of the unit these metrics are associated with is given via
    /// `unit_name`.
    fn append(&self, unit_name: &str, target: &mut metrics::Target) {
        target.append_simple(
            &Self::NUM_UPDATES_METRIC,
            Some(unit_name),
            self.num_updates.load(SeqCst),
        );

        target.append_simple(
            &Self::NUM_DROPPED_UPDATES_METRIC,
            Some(unit_name),
            self.num_dropped_updates.load(SeqCst),
        );

        match self.update.load() {
            Some(update) => {
                target.append_simple(&Self::UPDATE_WHEN_METRIC, Some(unit_name), update);
                let ago = Utc::now().signed_duration_since(update).num_seconds();
                target.append_simple(&Self::UPDATE_AGO_METRIC, Some(unit_name), ago);

                target.append_simple(
                    &Self::UPDATE_SET_SIZE_METRIC,
                    Some(unit_name),
                    self.update_set_size.load(SeqCst),
                );
            }
            None => {
                target.append_simple(&Self::UPDATE_WHEN_METRIC, Some(unit_name), "N/A");
                target.append_simple(&Self::UPDATE_AGO_METRIC, Some(unit_name), -1);
            }
        }
    }
}

//------------ GateStatus ----------------------------------------------------

/// The status of a gate.
#[derive(Debug, Default)]
pub enum GateStatus {
    /// The gate is connected to at least one active link.
    ///
    /// The unit owning this gate should produce updates.
    #[default]
    Active,

    /// The gate is not connected to any active links.
    ///
    /// This doesn't necessarily mean that there are no links at all, only
    /// that currently none of the links is interested in receiving updates
    /// from this unit.
    Dormant,

    /// The unit owning this gate has received a command via the manager.
    ///
    /// The payload contents have meaning only to the sender and receiver.
    ApplicationCommand { cmd: ApplicationCommand },
}

impl Eq for GateStatus {}

impl PartialEq for GateStatus {
    fn eq(&self, other: &Self) -> bool {
        // Auto-generated by Rust Analyzer
        core::mem::discriminant(self) == core::mem::discriminant(other)
    }
}

impl Display for GateStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GateStatus::Active => f.write_str("Active"),
            GateStatus::Dormant => f.write_str("Dormant"),
            GateStatus::ApplicationCommand { .. } => f.write_str("Triggered"),
        }
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

//------------ GateCommand ---------------------------------------------------

/// A command sent by a link to a gate.
#[derive(Debug)]
enum GateCommand {
    ApplicationCommand { data: ApplicationCommand },

    Terminate,
}

impl Clone for GateCommand {
    fn clone(&self) -> Self {
        match self {
            Self::Terminate => Self::Terminate,
            Self::ApplicationCommand { data } => Self::ApplicationCommand { data: data.clone() },
        }
    }
}

impl Display for GateCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GateCommand::ApplicationCommand { .. } => f.write_str("ApplicationCommand"),
            GateCommand::Terminate => f.write_str("Terminate"),
        }
    }
}

//------------ UpdateSender --------------------------------------------------

/// The gate side of sending updates.
#[derive(Clone, Debug)]
struct UpdateSender {
    /// The actual sender.
    ///
    /// This is an option to facilitate deleted dropped links. When sending
    /// fails, we swap this to `None` and then go over the slab again and
    /// drop anything that is `None`. We need to do this because
    /// `Slab::retain` isn’t async but `mpsc::Sender::send` is.
    queue: Option<mpsc::Sender<Result<Update, UnitStatus>>>,

    direct: Option<Weak<dyn AnyDirectUpdate>>,
}

//------------ UpdateReceiver ------------------------------------------------

/// The link side of receiving updates.
type UpdateReceiver = mpsc::Receiver<Result<Update, UnitStatus>>;

//------------ SubscribeResponse ---------------------------------------------

/// The response to a subscribe request.
#[derive(Debug)]
struct SubscribeResponse {
    /// The slot number of this subscription in the gate.
    slot: Uuid,

    /// The update receiver for this subscription.
    receiver: Option<UpdateReceiver>,
}
