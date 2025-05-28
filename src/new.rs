use std::sync::{Arc, Weak};

pub struct Center {
    // TODO: Can we get away without 'Arc's here?
    zone_loader: Arc<ZoneLoader>,
    key_manager: Arc<KeyManager>,
    signer: Arc<Signer>,
    review_server: Arc<Server>,
    outbound_server: Arc<Server>,

    zone_storage: Arc<ZoneStorage>,
    // TODO: Zone configuration data
}

pub enum CenterEvent {
    //--- Zone Loader ----------------------------------------------------------
    //
    /// A zone has been added, removed, or reconfigured.
    ///
    /// Conventional source: the zone loader or an external entity.
    ///
    /// Response:
    /// - Forward the update to all components.
    ZoneSetUpdated(ZoneSetUpdate),

    /// A version of a zone has been loaded.
    ///
    /// The new version of the zone is now present in the zone storage.  This
    /// zone is unsigned.
    ///
    /// Conventional source: the zone loader.
    ///
    /// Response:
    /// - If required, sign the zone.
    /// - Publish the zone in the review server.
    /// - Wait for the zone to be validated.
    /// - Publish the zone in the outbound server.
    ZoneLoaded {
        // TODO: Which zone is it?
        // TODO: What version of the zone is it?
    },

    // TODO:
    // - A specific version of a zone could not be loaded.
    // - The source of a zone became unavailable.

    //--- Key Manager ----------------------------------------------------------
    //
    /// A signing configuration for a zone has been selected.
    ///
    /// Conventional source: the key manager.
    ///
    /// Response:
    /// - Forward the update to the signer.
    ZoneSigningConfigSet(ZoneSigningConfig),

    /// A key rollover is waiting for a certain event to occur.
    ///
    /// Conventional source: the key manager.
    ///
    /// Once the specified event has occurred, [`Self::KeyRolloverEvent`] should
    /// be sent.
    KeyRolloverEventPending(RolloverEvent),

    /// A pending event for a key rollover has occurred.
    ///
    /// Conventional source: an external entity.
    ///
    /// Response:
    /// - Forward the event to the key manager.
    KeyRolloverEvent(RolloverEvent),

    //--- Signer ---------------------------------------------------------------
    //
    /// A zone has been signed.
    ///
    /// A loaded version of a zone has been signed, and the signed version is
    /// now present in the zone storage.
    ///
    /// Conventional source: the zone signer.
    ///
    /// Response:
    /// - Publish the zone in the review server.
    /// - Wait for the zone to be validated.
    /// - Publish the zone in the main server.
    ZoneSigned {
        // TODO: Which zone is it?
        // TODO: What is the version of the input zone this is based on?
        // TODO: What is the SOA version in this zone?
    },

    // TODO: Signing failures?

    //--- Servers --------------------------------------------------------------
    //
    /// A version of a zone is now published on a DNS server.
    ///
    /// Conventional source: the review/outbound server.
    ZonePublished {
        // TODO: Which zone is it?
        // TODO: What is the version of the input zone this is based on?
        // TODO: What is the SOA version in this zone?
        // TODO: Is it published on the review or public server?
    },

    /// A version of a zone has been reviewed and should be published.
    ///
    /// Conventional source: an external entity.
    ///
    /// Response:
    /// - Publish the zone in the outbound server.
    ZoneReviewed {
        // TODO: Which zone is it?
        // TODO: What is the version of the input zone this is based on?
        // TODO: What is the SOA version in this zone?
    },
}

/// An update to the existence or configuration of a zone.
///
/// All components should be notified of this update.
pub enum ZoneSetUpdate {
    /// A new zone has been registered.
    ///
    /// Configuration for the zone is now present in the global state.  There
    /// might not be a version of the zone that has been loaded yet.
    Registered {
        // TODO: Which zone is it?
    },

    /// The configuration for a zone has changed.
    ///
    /// New configuration for the zone is present in the global state.  It is
    /// unclear how different it is from the previous configuration.
    Reconfigured {
        // TODO: Which zone is it?
    },

    /// A zone has been removed.
    ///
    /// Configuration for the zone is going to be removed from the global state.
    Removed {
        // TODO: Which zone is it?
    },
}

/// Configuration for signing a zone.
pub struct ZoneSigningConfig {
    // TODO: Which zone is it?
    // TODO: Which keys to sign the zone with?
    // TODO: Special records (e.g. DNSKEY), with signatures.
}

/// A key rollover event for a zone.
pub enum RolloverEvent {
    /// A rollover step has been validated.
    ///
    /// An external entity (such as a human operator) has examined the specified
    /// rollover step and verified it, so that the key manager can now execute
    /// it.  Whether such validation is required depends on the configuration of
    /// the zone.
    StepValidated {
        // TODO: Which zone is it?
        // TODO: Which rollover step is it?
        // TODO: Which key does the step apply to?
    },

    /// A DNSKEY record has been propagated to all secondaries.
    ///
    /// A DNSKEY record has been propagated to the secondary servers of this
    /// zone, so that once the TTL of the previous DNSKEY record expires, the
    /// keys introduced in the new record can be used for signing.
    DnskeyPropagated {
        // TODO: Which zone is it?
        // TODO: What is the content of the DNSKEY record?
    },

    /// The parent of a zone has published a specific DS record.
    //
    // TODO: Lightly explain how this affects rollover.
    ParentHasDs {
        // TODO: Which zone is it?
        // TODO: What is the content of the DS record?
    },

    /// Signatures by certain keys have been propagated to all secondaries.
    ///
    /// RRSIG records generated using a specified set of keys have been
    /// propagated to the secondary servers of this zone.
    //
    // TODO: Lightly explain how this affects rollover.
    SignaturesPropagated {
        // TODO: Which zone is it?
        // TODO: Which keys' signatures are propagated?
    },

    /// A key has been generated for use in a certain zone.
    //
    // TODO: "KeySelected" instead?
    KeyGenerated {
        // TODO: Which zone is it?
        // TODO: Which key is it?
        // TODO: What can the key be used for (ZSK, KSK, CSK)?
    },
}

pub struct ZoneLoader {
    center: Weak<Center>,
}

impl ZoneLoader {
    /// Process an external change to the zone set.
    pub fn on_zone_set_updated(&self, update: ZoneSetUpdate) {
        todo!()
    }

    /// Start reloading a zone.
    pub fn reload_zone(&self) {
        todo!()
    }
}

pub struct KeyManager {
    center: Weak<Center>,
}

impl KeyManager {
    /// A pending key rollover event has been completed.
    pub fn on_rollover_event(&self, event: RolloverEvent) {
        todo!()
    }
}

pub struct Signer {
    center: Weak<Center>,
}

impl Signer {
    /// Sign a version of a zone.
    pub fn sign_zone(&self) {
        todo!()
    }
}

pub struct Server {
    center: Weak<Center>,
    // TODO: Is this a review or public server?
}

pub struct ZoneStorage {
    center: Weak<Center>,
}
