//! Saving Cascade's zone state.

use std::{
    collections::hash_map,
    fs,
    io::{self, BufReader},
    sync::Arc,
};

use camino::Utf8Path;
use serde::{Deserialize, Serialize};

use crate::{
    policy::{Policy, PolicyVersion},
    zone::{Zone, ZoneState},
};

pub mod v1;

//----------- ZoneStateSpec ----------------------------------------------------

/// A zone state file.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "version")]
pub enum Spec {
    /// The version 1 format.
    V1(v1::Spec),
}

//--- Conversion

impl Spec {
    /// Merge this specification with an existing zone state.
    pub fn parse_into(
        self,
        zone: &Arc<Zone>,
        state: &mut ZoneState,
        policies: &mut foldhash::HashMap<Box<str>, Policy>,
    ) {
        /// Synchronize a loaded policy with global state.
        fn sync_policy(
            policy: PolicyVersion,
            zone: &Arc<Zone>,
            policies: &mut foldhash::HashMap<Box<str>, Policy>,
        ) -> Arc<PolicyVersion> {
            // Check whether a policy of this name exists.
            match policies.entry(policy.name.clone()) {
                hash_map::Entry::Occupied(entry) => {
                    // A policy of this name exists.  Compare to it.
                    let existing = &entry.get().latest;
                    if **existing == policy {
                        return existing.clone();
                    }

                    // TODO: Continue using the older version of the policy, and
                    // enqueue an explicit change to the zone, so that any
                    // necessary hooks (e.g. re-signing) can be activated.
                    log::warn!(
                        "Zone '{}' is using an older version of policy '{}'; it will be updated",
                        zone.name,
                        policy.name
                    );
                    existing.clone()
                }

                hash_map::Entry::Vacant(entry) => {
                    log::warn!(
                        "Zone '{}' is using an unknown policy '{}'; the policy has been restored",
                        zone.name,
                        policy.name
                    );

                    let policy = Arc::new(policy);
                    entry.insert(Policy {
                        latest: policy.clone(),
                        mid_deletion: false,
                        zones: Default::default(),
                    });
                    policy
                }
            }
        }

        match self {
            Self::V1(v1::Spec { policy, source }) => {
                state.policy = policy.map(|policy| sync_policy(policy.parse(), zone, policies));
                state.source = source.map(|s| s.parse());
            }
        }
    }

    /// Build into this specification.
    pub fn build(zone: &ZoneState) -> Self {
        Self::V1(v1::Spec::build(zone))
    }
}

//--- Loading / Saving

impl Spec {
    /// Load and parse this specification from a file.
    pub fn load(path: &Utf8Path) -> io::Result<Self> {
        let file = BufReader::new(fs::File::open(path)?);
        let spec = serde_json::from_reader(file)?;
        Ok(spec)
    }

    /// Build and save this specification to a file.
    pub fn save(&self, path: &Utf8Path) -> io::Result<()> {
        let text = serde_json::to_string(self)?;
        crate::util::write_file(path, text.as_bytes())
    }
}
