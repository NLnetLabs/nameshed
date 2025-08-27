use std::sync::Arc;

use log::info;
use tokio::sync::mpsc;

use crate::center::Center;
use crate::comms::{ApplicationCommand, Terminated};
use crate::manager::TargetCommand;
use crate::payload::Update;

pub struct CentralCommand {
    pub center: Arc<Center>,
}

impl CentralCommand {
    pub async fn run(
        self,
        cmd_rx: mpsc::UnboundedReceiver<TargetCommand>,
        update_rx: mpsc::UnboundedReceiver<Update>,
    ) -> Result<(), Terminated> {
        let arc_self = Arc::new(self);

        arc_self.do_run(cmd_rx, update_rx).await
    }

    async fn do_run(
        self: &Arc<Self>,
        mut cmd_rx: mpsc::UnboundedReceiver<TargetCommand>,
        mut update_rx: mpsc::UnboundedReceiver<Update>,
    ) -> Result<(), Terminated> {
        loop {
            if let Err(Terminated) = self.process_events(&mut cmd_rx, &mut update_rx).await {
                // self.status_reporter.terminated();
                return Err(Terminated);
            }
        }
    }

    pub async fn process_events(
        self: &Arc<Self>,
        cmd_rx: &mut mpsc::UnboundedReceiver<TargetCommand>,
        update_rx: &mut mpsc::UnboundedReceiver<Update>,
    ) -> Result<(), Terminated> {
        loop {
            tokio::select! {
                // Disable tokio::select!() random branch selection
                biased;

                // If nothing happened above, check for new internal Rotonda
                // target commands to handle.
                cmd = cmd_rx.recv() => {
                    if let Some(_cmd) = &cmd {
                        // self.status_reporter.command_received(cmd);
                    }

                    match cmd {
                        None | Some(TargetCommand::Terminate) => {
                            return Err(Terminated);
                        }
                    }
                }

                Some(update) = update_rx.recv() => {
                    self.direct_update(update).await;
                }
            }
        }
    }
}

impl CentralCommand {
    async fn direct_update(&self, event: Update) {
        info!("[CC]: Event received: {event:?}");
        let (msg, target, cmd) = match event {
            Update::Changed(change) => {
                // Inform all units about the change.
                for name in ["ZL", "RS", "KM", "ZS", "RS2", "PS"] {
                    self.center
                        .app_cmd_tx
                        .send((name.into(), ApplicationCommand::Changed(change.clone())))
                        .unwrap();
                }
                return;
            }

            Update::RefreshZone {
                zone_name,
                source,
                serial,
            } => (
                "Instructing zone loader to refresh the zone",
                "ZL",
                ApplicationCommand::RefreshZone {
                    zone_name,
                    source,
                    serial,
                },
            ),

            Update::UnsignedZoneUpdatedEvent {
                zone_name,
                zone_serial,
            } => (
                "Instructing review server to publish the unsigned zone",
                "RS",
                ApplicationCommand::SeekApprovalForUnsignedZone {
                    zone_name,
                    zone_serial,
                },
            ),

            Update::UnsignedZoneApprovedEvent {
                zone_name,
                zone_serial,
            } => (
                "Instructing zone signer to sign the approved zone",
                "ZS",
                ApplicationCommand::SignZone {
                    zone_name,
                    zone_serial: Some(zone_serial),
                },
            ),

            Update::ResignZoneEvent { zone_name } => (
                "Instructing zone signer to re-sign the zone",
                "ZS",
                ApplicationCommand::SignZone {
                    zone_name,
                    zone_serial: None,
                },
            ),

            Update::ZoneSignedEvent {
                zone_name,
                zone_serial,
            } => (
                "Instructing review server to publish the signed zone",
                "RS2",
                ApplicationCommand::SeekApprovalForSignedZone {
                    zone_name,
                    zone_serial,
                },
            ),

            Update::SignedZoneApprovedEvent {
                zone_name,
                zone_serial,
            } => (
                "Instructing publication server to publish the signed zone",
                "PS",
                ApplicationCommand::PublishSignedZone {
                    zone_name,
                    zone_serial,
                },
            ),
        };

        info!("[CC]: {msg}");
        self.center.app_cmd_tx.send((target.into(), cmd)).unwrap();
    }
}

impl std::fmt::Debug for CentralCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CentralCommand").finish()
    }
}
