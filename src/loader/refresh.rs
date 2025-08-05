//! Monitoring refreshes.

use std::{
    borrow::{Borrow, BorrowMut},
    collections::BTreeSet,
    convert::Infallible,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::sync::watch;

use crate::zone::{self, Zone, ZoneByPtr};

use super::Loader;

//----------- RefreshMonitor ---------------------------------------------------

/// A monitor for zone refreshes.
pub struct RefreshMonitor {
    /// Scheduled refreshes.
    pub scheduled: Mutex<BTreeSet<ZoneByRefreshTime>>,

    /// The earliest refresh time.
    ///
    /// This is wrapped in a [`tokio::sync::watch`]er, so that the monitor's
    /// runner can watch for changes asynchronously.
    ///
    /// This should only be updated while 'scheduled' is locked, ensuring
    /// consistency between the two.
    pub earliest: watch::Sender<Option<Instant>>,
}

impl RefreshMonitor {
    /// Construct a new [`RefreshMonitor`].
    pub fn new() -> Self {
        Self {
            scheduled: Mutex::new(BTreeSet::new()),
            earliest: watch::Sender::new(None),
        }
    }

    /// Update a zone's scheduling.
    ///
    /// Given the old and new scheduled times, the refresh monitor's state will
    /// be updated, so that the zone is refreshed at the new scheduled time (if
    /// any).
    pub fn update(&self, zone: &Arc<Zone>, old: Option<Instant>, new: Option<Instant>) {
        let [old, new] = [old, new].map(|time| {
            time.map(|time| ZoneByRefreshTime {
                time,
                zone: ZoneByPtr(zone.clone()),
            })
        });

        // Lock the schedule.
        let mut scheduled = self
            .scheduled
            .lock()
            .expect("operations on 'scheduled' never panic");

        // Modify the schedule.
        match (old, new) {
            (None, None) => return,

            (Some(zone), None) => {
                // If the zone is not present in the schedule:
                // - The zone has already been ignored; or
                // - An internal inconsistency occurred.
                let _ = scheduled.remove(&zone);
            }

            (None, Some(zone)) => {
                // If the zone is already present in the schedule, an internal
                // inconsistency occurred.
                let _ = scheduled.insert(zone);
            }

            (Some(old), Some(new)) => {
                // If the zone is not present in the schedule:
                // - The zone has already been ignored; or
                // - An internal inconsistency occurred.
                let _ = scheduled.remove(&old);

                // If the zone is already present in the schedule, an internal
                // inconsistency occurred.
                let _ = scheduled.insert(new);
            }
        }

        // Update 'earliest'.
        self.earliest.send_if_modified(|value| {
            let new = scheduled.first().map(|zone| zone.time);
            let old = std::mem::replace(value, new);
            old != new
        });
    }

    /// Drive this refresh monitor.
    pub async fn run(&self, loader: &Arc<Loader>) -> Infallible {
        /// Wait until the specified instant.
        async fn wait(deadline: Option<Instant>) {
            if let Some(deadline) = deadline {
                tokio::time::sleep_until(deadline.into()).await
            } else {
                std::future::pending().await
            }
        }

        // A watcher for the 'earliest' field.
        let mut earliest_rx = self.earliest.subscribe();

        loop {
            // Extract the zones to refresh.
            let (earliest, now) = {
                // Lock the schedule.
                let mut scheduled = self
                    .scheduled
                    .lock()
                    .expect("operations on 'scheduled' never panic");

                // Extract any zones we want to refresh immediately.
                let latest = Instant::now() + Duration::from_secs(1);
                #[allow(clippy::mutable_key_type)]
                let later = scheduled.split_off(&latest);
                #[allow(clippy::mutable_key_type)]
                let now = std::mem::replace(&mut *scheduled, later);

                // Update 'earliest'.
                let earliest = scheduled.first().map(|zone| zone.time);
                self.earliest
                    .send(earliest)
                    .expect("'earliest_rx' is a connected receiver");
                earliest_rx.mark_unchanged(); // ignore this change locally.

                (earliest, now)
            };

            // Enqueue the relevant zone refreshes.
            for zone in now {
                let zone = zone.zone.0;

                let Ok(mut state) = zone.data.lock() else {
                    continue;
                };

                zone::LoaderState::enqueue_refresh(&mut state, &zone, false, loader);
            }

            // Wait for a refresh or a change to the schedule.
            tokio::select! {
                // Watch for external changes to the monitor.
                //
                // NOTE: The sender in 'self' exists so 'Err' is impossible.
                _ = earliest_rx.changed() => {},

                // Wait for the next scheduled zone refresh.
                () = wait(earliest) => {},
            }
        }
    }
}

impl Default for RefreshMonitor {
    fn default() -> Self {
        Self::new()
    }
}

//----------- ZoneByRefreshTime ------------------------------------------------

/// A [`Zone`] keyed by its refresh time.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ZoneByRefreshTime {
    /// When the zone needs to be refreshed.
    ///
    /// This is the moment of the earliest scheduled refresh for the zone,
    /// regardless of the reason this was scheduled (i.e. whether the previous
    /// refresh was successful or not).
    pub time: Instant,

    /// The zone in question.
    ///
    /// The zone is sorted by pointer address, so that different zones with the
    /// same scheduled refresh time are distinct and can be ordered.
    pub zone: ZoneByPtr,
}

impl AsRef<Instant> for ZoneByRefreshTime {
    fn as_ref(&self) -> &Instant {
        &self.time
    }
}

impl Borrow<Instant> for ZoneByRefreshTime {
    fn borrow(&self) -> &Instant {
        &self.time
    }
}

impl AsMut<Instant> for ZoneByRefreshTime {
    fn as_mut(&mut self) -> &mut Instant {
        &mut self.time
    }
}

impl BorrowMut<Instant> for ZoneByRefreshTime {
    fn borrow_mut(&mut self) -> &mut Instant {
        &mut self.time
    }
}
