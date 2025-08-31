//! Version 1 of the TSIG keys file.

use std::sync::Arc;

use domain::tsig;
use serde::{Deserialize, Serialize};

use crate::tsig::{TsigKey, TsigStore};

//----------- Spec -------------------------------------------------------------

/// A TSIG keys file.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Spec {
    /// A mapping of names to TSIG keys.
    pub map: foldhash::HashMap<tsig::KeyName, KeySpec>,
}

//--- Conversion

impl Spec {
    /// Parse from this specification.
    pub fn parse(self, store: &mut TsigStore) {
        // Write the loaded keys into a new hashmap, so keys that no longer
        // exist can be detected easily.
        let mut new_keys = foldhash::HashMap::default();

        // Traveres all the loaded keys.
        for (name, spec) in self.map {
            // Check for an existing key of this name.
            let key = if let Some(mut key) = store.map.remove(&name) {
                spec.parse_into(&mut key);
                key
            } else {
                log::info!("Loaded new TSIG key '{name}'");
                spec.parse(&name)
            };

            // Record the new key.
            let prev = new_keys.insert(name, key);
            assert!(prev.is_none(), "there is at most one key per name");
        }

        // Traverse keys that were no longer present.
        for (name, key) in store.map.drain() {
            // If any zones are using this key, keep it.
            if !key.zones.is_empty() {
                log::error!("The TSIG key '{name}' has been removed, but some zones are still using it; Cascade will preserve its internal copy");
                let prev = new_keys.insert(name, key);
                assert!(
                    prev.is_none(),
                    "'new_keys' and 'store.map' are disjoint sets"
                );
            } else {
                log::info!("Forgetting now-removed TSIG key '{name}'");
            }
        }

        // Update the set of keys.
        store.map = new_keys;
    }

    /// Build into this specification.
    pub fn build(store: &TsigStore) -> Self {
        Self {
            map: store
                .map
                .iter()
                .map(|(k, v)| (k.clone(), KeySpec::build(v)))
                .collect(),
        }
    }
}

//----------- KeySpec ----------------------------------------------------------

/// A TSIG key.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct KeySpec {
    /// The key algorithm.
    pub alg: AlgSpec,

    /// The private key material.
    pub data: Box<[u8]>,
}

//--- Conversion

impl KeySpec {
    /// Parse from this specification.
    pub fn parse(self, name: &tsig::KeyName) -> TsigKey {
        // TODO: Support custom min-mac-len and signing-len values?
        match tsig::Key::new(self.alg.parse(), &self.data, name.clone(), None, None) {
            Ok(key) => TsigKey {
                inner: Arc::new(key),
                material: self.data,
                zones: Default::default(),
            },
            Err(tsig::NewKeyError::BadMinMacLen) => unreachable!(),
            Err(tsig::NewKeyError::BadSigningLen) => unreachable!(),
        }
    }

    /// Merge an existing key with this specification.
    pub fn parse_into(self, dest: &mut TsigKey) {
        // TODO: Support custom min-mac-len and signing-len values?
        let name = dest.inner.name().clone();
        match tsig::Key::new(self.alg.parse(), &self.data, name, None, None) {
            Ok(key) => dest.inner = Arc::new(key),
            Err(tsig::NewKeyError::BadMinMacLen) => unreachable!(),
            Err(tsig::NewKeyError::BadSigningLen) => unreachable!(),
        }
    }

    /// Build into this specification.
    pub fn build(key: &TsigKey) -> Self {
        Self {
            alg: AlgSpec::build(key.inner.algorithm()),
            data: key.material.clone(),
        }
    }
}

//----------- AlgSpec ----------------------------------------------------------

/// A TSIG key algorithm specification.
#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AlgSpec {
    /// SHA-1.
    Sha1,

    /// SHA-256.
    Sha256,

    /// SHA-384,
    Sha384,

    /// SHA-512.
    Sha512,
}

//--- Conversion

impl AlgSpec {
    /// Parse from this specification.
    pub fn parse(self) -> tsig::Algorithm {
        match self {
            AlgSpec::Sha1 => tsig::Algorithm::Sha1,
            AlgSpec::Sha256 => tsig::Algorithm::Sha256,
            AlgSpec::Sha384 => tsig::Algorithm::Sha384,
            AlgSpec::Sha512 => tsig::Algorithm::Sha512,
        }
    }

    /// Build into this specification.
    pub fn build(alg: tsig::Algorithm) -> Self {
        match alg {
            tsig::Algorithm::Sha1 => AlgSpec::Sha1,
            tsig::Algorithm::Sha256 => AlgSpec::Sha256,
            tsig::Algorithm::Sha384 => AlgSpec::Sha384,
            tsig::Algorithm::Sha512 => AlgSpec::Sha512,
        }
    }
}
