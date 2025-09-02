//! Version 1 of the policy file.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::policy::{
    KeyManagerPolicy, LoaderPolicy, Nsec3OptOutPolicy, PolicyVersion, ReviewPolicy, ServerPolicy,
    SignerDenialPolicy, SignerPolicy, SignerSerialPolicy,
};

//----------- Spec -------------------------------------------------------------

/// A policy file.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct Spec {
    /// How zones are loaded.
    pub loader: LoaderSpec,

    /// Zone key management.
    pub key_manager: KeyManagerSpec,

    /// How zones are signed.
    pub signer: SignerSpec,

    /// How zones are served.
    pub server: ServerSpec,
}

//--- Conversion

impl Spec {
    /// Parse from this specification.
    pub fn parse(self, name: &str) -> PolicyVersion {
        PolicyVersion {
            name: name.into(),
            loader: self.loader.parse(),
            key_manager: self.key_manager.parse(),
            signer: self.signer.parse(),
            server: self.server.parse(),
        }
    }

    /// Build into this specification.
    pub fn build(policy: &PolicyVersion) -> Self {
        Self {
            loader: LoaderSpec::build(&policy.loader),
            key_manager: KeyManagerSpec::build(&policy.key_manager),
            signer: SignerSpec::build(&policy.signer),
            server: ServerSpec::build(&policy.server),
        }
    }
}

//----------- LoaderSpec -------------------------------------------------------

/// Policy for loading zones.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct LoaderSpec {
    /// Reviewing loaded zones.
    pub review: ReviewSpec,
}

//--- Conversion

impl LoaderSpec {
    /// Parse from this specification.
    pub fn parse(self) -> LoaderPolicy {
        LoaderPolicy {
            review: self.review.parse(),
        }
    }

    /// Build into this specification.
    pub fn build(policy: &LoaderPolicy) -> Self {
        Self {
            review: ReviewSpec::build(&policy.review),
        }
    }
}

//----------- KeyManagerSpec ---------------------------------------------------

/// Policy for zone key management.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct KeyManagerSpec {}

//--- Conversion

impl KeyManagerSpec {
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

//----------- SignerSpec -------------------------------------------------------

/// Policy for signing zones.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct SignerSpec {
    /// The serial number generation policy.
    pub serial_policy: SignerSerialPolicySpec,

    /// The offset for record signature inceptions, in seconds.
    pub sig_inception_offset: u64,

    /// How long record signatures will be valid for, in seconds.
    pub sig_validity_time: u64,

    /// How denial-of-existence records are generated.
    pub denial: SignerDenialSpec,

    /// Reviewing signed zones.
    pub review: ReviewSpec,
    //
    // TODO:
    // - Signing policy (disabled, pass-through?, enabled)
}

//--- Conversion

impl SignerSpec {
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
            denial: SignerDenialSpec::build(&policy.denial),
            review: ReviewSpec::build(&policy.review),
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

//----------- SignerDenialSpec -------------------------------------------------

/// Spec for generating denial-of-existence records.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, tag = "type")]
pub enum SignerDenialSpec {
    /// Generate NSEC records.
    NSec,

    /// Generate NSEC3 records.
    NSec3 {
        /// Whether and how to enable NSEC3 Opt-Out.
        opt_out: Nsec3OptOutSpec,
    },
}

//--- Conversion

impl SignerDenialSpec {
    /// Parse from this specification.
    pub fn parse(self) -> SignerDenialPolicy {
        match self {
            SignerDenialSpec::NSec => SignerDenialPolicy::NSec,
            SignerDenialSpec::NSec3 { opt_out } => SignerDenialPolicy::NSec3 {
                opt_out: opt_out.parse(),
            },
        }
    }

    /// Build into this specification.
    pub fn build(policy: &SignerDenialPolicy) -> Self {
        match *policy {
            SignerDenialPolicy::NSec => SignerDenialSpec::NSec,
            SignerDenialPolicy::NSec3 { opt_out } => SignerDenialSpec::NSec3 {
                opt_out: Nsec3OptOutSpec::build(opt_out),
            },
        }
    }
}

//--- Default

impl Default for SignerDenialSpec {
    fn default() -> Self {
        Self::NSec3 {
            opt_out: Nsec3OptOutSpec::Disabled,
        }
    }
}

//----------- Nsec3OptOutSpec --------------------------------------------------

/// Spec for the NSEC3 Opt-Out mechanism.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, tag = "type")]
pub enum Nsec3OptOutSpec {
    /// Do not enable Opt-Out.
    #[default]
    Disabled,

    /// Only set the Opt-Out flag.
    FlagOnly,

    /// Enable Opt-Out and omit the corresponding NSEC3 records.
    Enabled,
}

//--- Conversion

impl Nsec3OptOutSpec {
    /// Parse from this specification.
    pub fn parse(self) -> Nsec3OptOutPolicy {
        match self {
            Nsec3OptOutSpec::Disabled => Nsec3OptOutPolicy::Disabled,
            Nsec3OptOutSpec::FlagOnly => Nsec3OptOutPolicy::FlagOnly,
            Nsec3OptOutSpec::Enabled => Nsec3OptOutPolicy::Enabled,
        }
    }

    /// Build into this specification.
    pub fn build(policy: Nsec3OptOutPolicy) -> Self {
        match policy {
            Nsec3OptOutPolicy::Disabled => Nsec3OptOutSpec::Disabled,
            Nsec3OptOutPolicy::FlagOnly => Nsec3OptOutSpec::FlagOnly,
            Nsec3OptOutPolicy::Enabled => Nsec3OptOutSpec::Enabled,
        }
    }
}

//----------- ReviewSpec -------------------------------------------------------

/// Policy for reviewing loaded/signed zones.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct ReviewSpec {
    /// Whether review is required.
    pub required: bool,

    /// A command hook for reviewing a new version of the zone.
    pub cmd_hook: Option<String>,
}

//--- Conversion

impl ReviewSpec {
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
pub struct ServerSpec {}

//--- Conversion

impl ServerSpec {
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
