//! The policy file.

use std::{fs, io, sync::Arc};

use camino::Utf8Path;
use serde::{Deserialize, Serialize};

use super::Policy;

pub mod v1;

//----------- FileSpec ---------------------------------------------------------

/// A policy file.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "version")]
pub enum Spec {
    /// The version 1 format.
    V1(v1::Spec),
}

//--- Conversion

impl Spec {
    /// Parse a new [`Policy`].
    pub fn parse(self, name: &str) -> Policy {
        let latest = Arc::new(match self {
            Self::V1(spec) => spec.parse(name),
        });

        Policy {
            latest: latest.clone(),
            mid_deletion: false,
            zones: Default::default(),
        }
    }

    /// Merge with an existing [`Policy`].
    pub fn parse_into(self, existing: &mut Policy) {
        let latest = &*existing.latest;
        let new = match self {
            Self::V1(spec) => spec.parse(&latest.name),
        };

        if *latest == new {
            // The policy has not changed.
            return;
        }

        // The policy has changed.
        existing.latest = Arc::new(new);
        // TODO: Enqueue policy refreshes for associated zones?
    }

    /// Build into this specification.
    pub fn build(policy: &Policy) -> Self {
        Self::V1(v1::Spec::build(&policy.latest))
    }
}

//--- Loading / Saving

impl Spec {
    /// Load and parse this specification from a file.
    pub fn load(path: &Utf8Path) -> io::Result<Self> {
        let text = fs::read_to_string(path)?;
        toml::from_str(&text).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }

    /// Build and save this specification to a file.
    pub fn save(&self, path: &Utf8Path) -> io::Result<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        fs::write(path, text)
    }
}
