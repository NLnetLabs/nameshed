//! Version 1 of the TSIG keys file.

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
        store.map.clear();
        for (name, key) in self.map {
            let key = key.parse(&name);
            store.map.insert(name, key);
        }
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
                inner: key,
                material: self.data,
            },
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
