//! Zone policy.

use std::{fs, io, sync::Arc, time::Duration};

use bytes::Bytes;
use camino::Utf8PathBuf;
use domain::base::Name;

use crate::config::Config;

pub mod file;

//----------- Policy -----------------------------------------------------------

/// A policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// The latest version of the policy.
    pub latest: Arc<PolicyVersion>,

    /// Whether the policy is being deleted.
    ///
    /// This is an intermediate state used to prevent race conditions while the
    /// policy is being removed.  In this state, new zones cannot be attached to
    /// this policy.
    pub mid_deletion: bool,

    /// The zones using this policy.
    pub zones: foldhash::HashSet<Name<Bytes>>,
}

//--- Loading / Saving

impl Policy {
    /// Reload this policy.
    pub fn reload(&mut self, config: &Config) -> io::Result<()> {
        // TODO: Carefully consider how 'config.policy_dir' and the path last
        // loaded from are synchronized.

        let path = config.policy_dir.join(format!("{}.toml", self.latest.name));
        file::Spec::load(&path)?.parse_into(self);
        Ok(())
    }
}

/// Reload all policies.
pub fn reload_all(
    policies: &mut foldhash::HashMap<Box<str>, Policy>,
    config: &Config,
) -> io::Result<()> {
    // Write the loaded policies to a new hashmap, so policies that no longer
    // exist can be detected easily.
    let mut new_policies = foldhash::HashMap::<_, _>::default();

    // Traverse all objects in the policy directory.
    for entry in fs::read_dir(&*config.policy_dir)? {
        let entry = entry?;

        // Filter for UTF-8 paths.
        let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) else {
            log::warn!(
                "Ignoring potential policy file '{}' as the path is non-UTF-8",
                entry.path().display()
            );
            continue;
        };

        // Filter for '.toml' files.
        if path.extension() != Some("toml") {
            continue;
        }

        // Try loading the file; ignore a failure if it's a directory.
        //
        // NOTE: Checking that the object is a file, and then opening it, would
        // be vulnerable to TOCTOU.
        let spec = match file::Spec::load(&path) {
            Ok(spec) => spec,
            // Ignore a directory ending in '.toml'.
            Err(err) if err.kind() == io::ErrorKind::IsADirectory => continue,
            Err(err) => return Err(err),
        };

        // Build a new policy or merge an existing one.
        let name = path
            .file_stem()
            .expect("this path points to a readable file, so it must have a file name");
        let policy = if let Some(mut policy) = policies.remove(name) {
            spec.parse_into(&mut policy);
            policy
        } else {
            log::info!("Loaded new policy '{name}'");

            spec.parse(name)
        };

        // Record the new policy.
        let prev = new_policies.insert(name.into(), policy);
        assert!(prev.is_none(), "there is at most one policy per path");
    }

    // Traverse policies whose files were not found.
    for (name, policy) in policies.drain() {
        // If any zones are using this policy, keep it.
        if !policy.zones.is_empty() {
            log::error!("The file backing policy '{name}' has been removed, but some zones are still using it; Cascade will preserve its internal copy");
            let prev = new_policies.insert(name, policy);
            assert!(
                prev.is_none(),
                "'new_policies' and 'policies' are disjoint sets"
            );
        } else {
            log::info!("Forgetting now-removed policy '{name}'");
        }
    }

    // Update the set of policies.
    *policies = new_policies;

    Ok(())
}

//----------- PolicyVersion ----------------------------------------------------

/// A particular version of a policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyVersion {
    /// The name of the policy.
    pub name: Box<str>,

    /// How zones are loaded.
    pub loader: LoaderPolicy,

    /// Zone key management.
    pub key_manager: KeyManagerPolicy,

    /// How zones are signed.
    pub signer: SignerPolicy,

    /// How zones are served.
    pub server: ServerPolicy,
}

//----------- LoaderPolicy -----------------------------------------------------

/// Policy for loading zones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoaderPolicy {
    /// Reviewing loaded zones.
    pub review: ReviewPolicy,
}

//----------- KeyManagerPolicy -------------------------------------------------

/// Policy for zone key management.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyManagerPolicy {}

//----------- SignerPolicy -----------------------------------------------------

/// Policy for signing zones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignerPolicy {
    /// The serial number generation policy.
    ///
    /// This is used to generate new SOA serial numbers, inserted into zones
    /// being (re)signed.
    pub serial_policy: SignerSerialPolicy,

    /// The offset for record signature inceptions.
    ///
    /// When DNS records are signed, the `RRSIG` signature records will record
    /// that the signature was made this far in the past.  This can help DNSSEC
    /// validation pass in case the signer and validator disagree on the current
    /// time (by a small amount).
    pub sig_inception_offset: Duration,

    /// How long record signatures will be valid for.
    pub sig_validity_time: Duration,

    /// How denial-of-existence records are generated.
    pub denial: SignerDenialPolicy,

    /// Reviewing signed zones.
    pub review: ReviewPolicy,
    //
    // TODO:
    // - Signing policy (disabled, pass-through?, enabled)
    // - Support keeping unsigned vs. signed zone serials distinct
}

//----------- SignerSerialPolicy -----------------------------------------------

/// Policy for generating serial numbers.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SignerSerialPolicy {
    /// Use the same serial number as the unsigned zone.
    ///
    /// The zone cannot be resigned, not without a change in the underlying
    /// unsigned contents.
    Keep,

    /// Increment the serial number on every change.
    Counter,

    /// Use the current Unix time, in seconds.
    ///
    /// New versions of the zone cannot be generated in the same second.
    UnixTime,

    /// Set the serial number to `<YYYY><MM><DD><xx>`.
    ///
    /// The serial number, when formatted in decimal, contains the calendar
    /// date (in the UTC timezone).  The `<xx>` component is a simple counter;
    /// at most 100 versions of the zone can be used per day.
    //
    // TODO: How to handle "emergency" situations where the zone will expire?
    DateCounter,
}

//----------- SignerDenialPolicy -----------------------------------------------

/// Policy for generating denial-of-existence records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignerDenialPolicy {
    /// Generate NSEC records.
    NSec,

    /// Generate NSEC3 records.
    NSec3 {
        /// Whether and how to enable NSEC3 Opt-Out.
        opt_out: Nsec3OptOutPolicy,
    },
}

//----------- Nsec3OptOutPolicy ------------------------------------------------

/// Policy for the NSEC3 Opt-Out mechanism.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Nsec3OptOutPolicy {
    /// Do not enable Opt-Out.
    Disabled,

    /// Only set the Opt-Out flag.
    FlagOnly,

    /// Enable Opt-Out and omit the corresponding NSEC3 records.
    Enabled,
}

//----------- ReviewPolicy -----------------------------------------------------

/// Policy for reviewing loaded/signed zones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewPolicy {
    /// Whether review is required.
    ///
    /// If this is `false`, zones under this policy will not wait for external
    /// approval of new versions when they are loaded / signed.
    pub required: bool,

    /// A command hook for reviewing a new version of the zone.
    ///
    /// When a new loaded / signed version of the zone is prepared, this hook
    /// (if [`Some`]) will be spawned to verify the zone.  If review is required
    /// and the hook fails, the zone will not be propagated.
    pub cmd_hook: Option<String>,
}

//----------- ServerPolicy -----------------------------------------------------

/// Policy for serving zones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerPolicy {}
