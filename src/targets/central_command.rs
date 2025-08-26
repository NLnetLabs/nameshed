use std::fmt::Debug;
use std::sync::Arc;

use arc_swap::ArcSwap;
use log::info;
use serde::Deserialize;
use serde_with::serde_as;
use tokio::sync::mpsc;

use crate::comms::{ApplicationCommand, Terminated};
use crate::manager::{Component, TargetCommand};
use crate::payload::Update;

#[derive(Debug)]
pub struct CentralCommandTarget {
    /// A receiver for updates.
    pub update_rx: mpsc::Receiver<Update>,

    pub config: Config,
}

impl CentralCommandTarget {
    pub async fn run(
        self,
        component: Component,
        cmd: mpsc::Receiver<TargetCommand>,
    ) -> Result<(), Terminated> {
        CentralCommand::new(self.config, component)
            .run(cmd, self.update_rx)
            .await
    }
}

pub(super) struct CentralCommand {
    component: Component,
    config: Arc<ArcSwap<Config>>,
}

impl CentralCommand {
    pub fn new(config: Config, component: Component) -> Self {
        let config = Arc::new(ArcSwap::from_pointee(config));

        // TODO: metrics and status reporting

        Self { component, config }
    }

    pub async fn run(
        mut self,
        cmd_rx: mpsc::Receiver<TargetCommand>,
        update_rx: mpsc::Receiver<Update>,
    ) -> Result<(), Terminated> {
        let _component = &mut self.component;

        let arc_self = Arc::new(self);

        arc_self.do_run(cmd_rx, update_rx).await
    }

    pub async fn do_run(
        self: &Arc<Self>,
        mut cmd_rx: mpsc::Receiver<TargetCommand>,
        mut update_rx: mpsc::Receiver<Update>,
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
        cmd_rx: &mut mpsc::Receiver<TargetCommand>,
        update_rx: &mut mpsc::Receiver<Update>,
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
        self.component.send_command(target, cmd).await;
    }
}

impl std::fmt::Debug for CentralCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CentralCommand").finish()
    }
}

//------------ Config --------------------------------------------------------

#[serde_as]
#[derive(Debug, Default, Deserialize)]
pub struct Config {}
