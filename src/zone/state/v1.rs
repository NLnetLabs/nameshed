//! Version 1 of the zone state file.

use std::{net::SocketAddr, time::Duration};

use camino::Utf8Path;
use serde::{Deserialize, Serialize};

use crate::{
    api::ZoneSource,
    policy::{
        KeyManagerPolicy, LoaderPolicy, Nsec3OptOutPolicy, PolicyVersion, ReviewPolicy,
        ServerPolicy, SignerDenialPolicy, SignerPolicy, SignerSerialPolicy,
    },
    zone::ZoneState,
};

//----------- Spec -------------------------------------------------------------

/// A zone state file.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Spec {
    /// The current policy.
    ///
    /// The full details of the policy are stored here, as there may be a newer
    /// version of the policy that is not yet in use.
    pub policy: Option<PolicySpec>,

    /// The source to load the zone from
    pub source: Option<ZoneSourceSpec>,
}

//--- Conversion

impl Spec {
    /// Build into this specification.
    pub fn build(zone: &ZoneState) -> Self {
        Self {
            policy: zone.policy.as_ref().map(|p| PolicySpec::build(p)),
            source: zone.source.as_ref().map(|s| ZoneSourceSpec::build(s)),
        }
    }
}

//----------- PolicySpec -------------------------------------------------------

/// The policy details for a zone.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PolicySpec {
    /// The name of the policy.
    pub name: Box<str>,

    /// How zones are loaded.
    pub loader: LoaderPolicySpec,

    /// Zone key management.
    pub key_manager: KeyManagerPolicySpec,

    /// How zones are signed.
    pub signer: SignerPolicySpec,

    /// How zones are served.
    pub server: ServerPolicySpec,
}

//--- Conversion

impl PolicySpec {
    /// Parse from this specification.
    pub fn parse(self) -> PolicyVersion {
        PolicyVersion {
            name: self.name,
            loader: self.loader.parse(),
            key_manager: self.key_manager.parse(),
            signer: self.signer.parse(),
            server: self.server.parse(),
        }
    }

    /// Build into this specification.
    pub fn build(policy: &PolicyVersion) -> Self {
        Self {
            name: policy.name.clone(),
            loader: LoaderPolicySpec::build(&policy.loader),
            key_manager: KeyManagerPolicySpec::build(&policy.key_manager),
            signer: SignerPolicySpec::build(&policy.signer),
            server: ServerPolicySpec::build(&policy.server),
        }
    }
}

//----------- ZoneSourceSpec ---------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum ZoneSourceSpec {
    Zonefile { path: Box<Utf8Path> },
    Server { addr: SocketAddr },
}

impl ZoneSourceSpec {
    /// Parse from this specification.
    pub fn parse(self) -> ZoneSource {
        match self {
            ZoneSourceSpec::Zonefile { path } => ZoneSource::Zonefile { path },
            ZoneSourceSpec::Server { addr } => ZoneSource::Server { addr },
        }
    }

    /// Build into this specification.
    pub fn build(policy: &ZoneSource) -> Self {
        match policy {
            ZoneSource::Zonefile { path } => ZoneSourceSpec::Zonefile { path: path.clone() },
            ZoneSource::Server { addr } => ZoneSourceSpec::Server { addr: addr.clone() },
        }
    }
}

//----------- LoaderSpec -------------------------------------------------------

/// Policy for loading zones.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct LoaderPolicySpec {
    /// Reviewing loaded zones.
    pub review: ReviewPolicySpec,
}

//--- Conversion

impl LoaderPolicySpec {
    /// Parse from this specification.
    pub fn parse(self) -> LoaderPolicy {
        LoaderPolicy {
            review: self.review.parse(),
        }
    }

    /// Build into this specification.
    pub fn build(policy: &LoaderPolicy) -> Self {
        Self {
            review: ReviewPolicySpec::build(&policy.review),
        }
    }
}

//----------- KeyManagerSpec ---------------------------------------------------

/// Policy for zone key management.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct KeyManagerPolicySpec {}

//--- Conversion

impl KeyManagerPolicySpec {
    /// Parse from this specification.
    pub fn parse(self) -> KeyManagerPolicy {
        KeyManagerPolicy {}
    }

    /// Build into this specification.
    pub fn build(policy: &KeyManagerPolicy) -> Self {
        let KeyManagerPolicy {} = policy;
        Self {}
    }
}

//----------- SignerPolicySpec -------------------------------------------------

/// Policy for signing zones.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct SignerPolicySpec {
    /// The serial number generation policy.
    pub serial_policy: SignerSerialPolicySpec,

    /// The offset for record signature inceptions, in seconds.
    pub sig_inception_offset: u64,

    /// How long record signatures will be valid for, in seconds.
    pub sig_validity_time: u64,

    /// How denial-of-existence records are generated.
    pub denial: SignerDenialPolicySpec,

    /// Reviewing signed zones.
    pub review: ReviewPolicySpec,
}

//--- Conversion

impl SignerPolicySpec {
    /// Parse from this specification.
    pub fn parse(self) -> SignerPolicy {
        SignerPolicy {
            serial_policy: self.serial_policy.parse(),
            sig_inception_offset: Duration::from_secs(self.sig_inception_offset),
            sig_validity_time: Duration::from_secs(self.sig_validity_time),
            denial: self.denial.parse(),
            review: self.review.parse(),
        }
    }

    /// Build into this specification.
    pub fn build(policy: &SignerPolicy) -> Self {
        Self {
            serial_policy: SignerSerialPolicySpec::build(policy.serial_policy),
            sig_inception_offset: policy.sig_inception_offset.as_secs(),
            sig_validity_time: policy.sig_validity_time.as_secs(),
            denial: SignerDenialPolicySpec::build(&policy.denial),
            review: ReviewPolicySpec::build(&policy.review),
        }
    }
}

//----------- SignerSerialPolicySpec -------------------------------------------

/// Policy for generating serial numbers.
#[derive(Copy, Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum SignerSerialPolicySpec {
    /// Use the same serial number as the unsigned zone.
    Keep,

    /// Increment the serial number on every change.
    #[default]
    Counter,

    /// Use the current Unix time, in seconds.
    ///
    /// New versions of the zone cannot be generated in the same second.
    UnixTime,

    /// Set the serial number to `<YYYY><MM><DD><xx>`.
    DateCounter,
}

//--- Conversion

impl SignerSerialPolicySpec {
    /// Parse from this specification.
    pub fn parse(self) -> SignerSerialPolicy {
        match self {
            Self::Keep => SignerSerialPolicy::Keep,
            Self::Counter => SignerSerialPolicy::Counter,
            Self::UnixTime => SignerSerialPolicy::UnixTime,
            Self::DateCounter => SignerSerialPolicy::DateCounter,
        }
    }

    /// Build into this specification.
    pub fn build(policy: SignerSerialPolicy) -> Self {
        match policy {
            SignerSerialPolicy::Keep => Self::Keep,
            SignerSerialPolicy::Counter => Self::Counter,
            SignerSerialPolicy::UnixTime => Self::UnixTime,
            SignerSerialPolicy::DateCounter => Self::DateCounter,
        }
    }
}

//----------- SignerDenialPolicySpec -------------------------------------------

/// Spec for generating denial-of-existence records.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, tag = "type")]
pub enum SignerDenialPolicySpec {
    /// Generate NSEC records.
    NSec,

    /// Generate NSEC3 records.
    NSec3 {
        /// Whether and how to enable NSEC3 Opt-Out.
        opt_out: Nsec3OptOutPolicySpec,
    },
}

//--- Conversion

impl SignerDenialPolicySpec {
    /// Parse from this specification.
    pub fn parse(self) -> SignerDenialPolicy {
        match self {
            SignerDenialPolicySpec::NSec => SignerDenialPolicy::NSec,
            SignerDenialPolicySpec::NSec3 { opt_out } => SignerDenialPolicy::NSec3 {
                opt_out: opt_out.parse(),
            },
        }
    }

    /// Build into this specification.
    pub fn build(policy: &SignerDenialPolicy) -> Self {
        match *policy {
            SignerDenialPolicy::NSec => SignerDenialPolicySpec::NSec,
            SignerDenialPolicy::NSec3 { opt_out } => SignerDenialPolicySpec::NSec3 {
                opt_out: Nsec3OptOutPolicySpec::build(opt_out),
            },
        }
    }
}

//--- Default

impl Default for SignerDenialPolicySpec {
    fn default() -> Self {
        Self::NSec3 {
            opt_out: Nsec3OptOutPolicySpec::Disabled,
        }
    }
}

//----------- Nsec3OptOutPolicySpec --------------------------------------------

/// Spec for the NSEC3 Opt-Out mechanism.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, tag = "type")]
pub enum Nsec3OptOutPolicySpec {
    /// Do not enable Opt-Out.
    #[default]
    Disabled,

    /// Only set the Opt-Out flag.
    FlagOnly,

    /// Enable Opt-Out and omit the corresponding NSEC3 records.
    Enabled,
}

//--- Conversion

impl Nsec3OptOutPolicySpec {
    /// Parse from this specification.
    pub fn parse(self) -> Nsec3OptOutPolicy {
        match self {
            Nsec3OptOutPolicySpec::Disabled => Nsec3OptOutPolicy::Disabled,
            Nsec3OptOutPolicySpec::FlagOnly => Nsec3OptOutPolicy::FlagOnly,
            Nsec3OptOutPolicySpec::Enabled => Nsec3OptOutPolicy::Enabled,
        }
    }

    /// Build into this specification.
    pub fn build(policy: Nsec3OptOutPolicy) -> Self {
        match policy {
            Nsec3OptOutPolicy::Disabled => Nsec3OptOutPolicySpec::Disabled,
            Nsec3OptOutPolicy::FlagOnly => Nsec3OptOutPolicySpec::FlagOnly,
            Nsec3OptOutPolicy::Enabled => Nsec3OptOutPolicySpec::Enabled,
        }
    }
}

//----------- ReviewSpec -------------------------------------------------------

/// Policy for reviewing loaded/signed zones.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct ReviewPolicySpec {
    /// Whether review is required.
    pub required: bool,

    /// A command hook for reviewing a new version of the zone.
    pub cmd_hook: Option<String>,
}

//--- Conversion

impl ReviewPolicySpec {
    /// Parse from this specification.
    pub fn parse(self) -> ReviewPolicy {
        ReviewPolicy {
            required: self.required,
            cmd_hook: self.cmd_hook,
        }
    }

    /// Build into this specification.
    pub fn build(policy: &ReviewPolicy) -> Self {
        Self {
            required: policy.required,
            cmd_hook: policy.cmd_hook.clone(),
        }
    }
}

//----------- ServerSpec -------------------------------------------------------

/// Policy for serving zones.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct ServerPolicySpec {}

//--- Conversion

impl ServerPolicySpec {
    /// Parse from this specification.
    pub fn parse(self) -> ServerPolicy {
        ServerPolicy {}
    }

    /// Build into this specification.
    pub fn build(policy: &ServerPolicy) -> Self {
        let ServerPolicy {} = policy;
        Self {}
    }
}
