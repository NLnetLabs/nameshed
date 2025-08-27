//! Version 1 of the policy file.

use serde::{Deserialize, Serialize};

use crate::policy::{
    KeyManagerPolicy, LoaderPolicy, PolicyVersion, ReviewPolicy, ServerPolicy, SignerPolicy,
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
    /// Reviewing signed zones.
    pub review: ReviewSpec,
}

//--- Conversion

impl SignerSpec {
    /// Parse from this specification.
    pub fn parse(self) -> SignerPolicy {
        SignerPolicy {
            review: self.review.parse(),
        }
    }

    /// Build into this specification.
    pub fn build(policy: &SignerPolicy) -> Self {
        Self {
            review: ReviewSpec::build(&policy.review),
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
