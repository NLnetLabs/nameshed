use core::str::FromStr;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use domain::base::name::FromStrError;
use domain::base::ToName;
use domain::tsig::{Algorithm, AlgorithmError, Key, KeyName, KeyStore, NewKeyError};
use domain::utils::base64::{self, DecodeError};
use log::info;

pub type KeyId = (KeyName, Algorithm);

#[derive(Clone, Debug, Default)]
pub struct TsigKeyStore {
    inner: Inner,
}

impl TsigKeyStore {
    pub fn new() -> Self {
        Self {
            inner: Default::default(),
        }
    }
}

impl TsigKeyStore {
    pub fn insert(&self, key: Key) -> bool {
        let key_id = (key.name().to_owned(), key.algorithm());
        if let Ok(mut store) = self.inner.0.write() {
            store.insert(key_id, key).is_none()
        } else {
            false
        }
    }

    pub fn get_key<N: ToName>(&self, name: &N, algorithm: Algorithm) -> Option<Key> {
        self.inner.get_key(name, algorithm)
    }

    pub fn get_key_by_name(&self, encoded_key_name: &KeyName) -> Option<Key> {
        if let Ok(store) = self.inner.0.read() {
            return store
                .iter()
                .find_map(|((key_name, _alg), key)| {
                    if key_name == encoded_key_name {
                        Some(key)
                    } else {
                        None
                    }
                })
                .cloned();
        }
        None
    }
}

impl AsRef<Key> for TsigKeyStore {
    fn as_ref(&self) -> &Key {
        todo!()
    }
}

impl std::ops::Deref for TsigKeyStore {
    type Target = Inner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[derive(Clone, Debug, Default)]
pub struct Inner(Arc<RwLock<HashMap<(KeyName, Algorithm), Key>>>);

impl KeyStore for Inner {
    type Key = domain::tsig::Key;

    fn get_key<N: ToName>(&self, name: &N, algorithm: Algorithm) -> Option<Self::Key> {
        if let Ok(key_name) = name.try_to_name() {
            let key = (key_name, algorithm);
            if let Ok(store) = self.0.read() {
                return store.get(&key).cloned();
            }
        }
        None
    }
}

pub enum KeyParseError {
    InvalidAlgorithm,

    /// TSIG key string must have the form [<algorithm>]:<base64 bytes>
    InvalidStructure,

    InvalidName(FromStrError),

    InvalidBase64(DecodeError),

    KeyCreationError(NewKeyError),
}

impl std::fmt::Display for KeyParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyParseError::InvalidAlgorithm => f.write_str("InvalidAlgorithm"),
            KeyParseError::InvalidStructure => f.write_str("InvalidStructure"),
            KeyParseError::InvalidName(err) => f.write_fmt(format_args!("InvalidName: {err}")),
            KeyParseError::InvalidBase64(err) => f.write_fmt(format_args!("InvalidBase64: {err}")),
            KeyParseError::KeyCreationError(err) => {
                f.write_fmt(format_args!("KeyCreationError: {err}"))
            }
        }
    }
}

impl From<AlgorithmError> for KeyParseError {
    fn from(_: AlgorithmError) -> Self {
        Self::InvalidAlgorithm
    }
}

impl From<FromStrError> for KeyParseError {
    fn from(err: FromStrError) -> Self {
        Self::InvalidName(err)
    }
}

impl From<DecodeError> for KeyParseError {
    fn from(err: DecodeError) -> Self {
        Self::InvalidBase64(err)
    }
}

impl From<NewKeyError> for KeyParseError {
    fn from(err: NewKeyError) -> Self {
        Self::KeyCreationError(err)
    }
}

pub fn parse_key_strings(name: &str, alg_and_hex_key_bytes: &str) -> Result<Key, KeyParseError> {
    let key_parts: Vec<String> = alg_and_hex_key_bytes
        .split(':')
        .map(ToString::to_string)
        .collect();

    let (alg, base64) = match key_parts.len() {
        1 => (Algorithm::Sha256, key_parts[0].clone()),
        2 => {
            let alg = Algorithm::from_str(&key_parts[0])?;
            (alg, key_parts[1].clone())
        }
        _ => return Err(KeyParseError::InvalidStructure),
    };

    let encoded_key_name = KeyName::from_str(name)?;
    let secret = base64::decode::<Vec<u8>>(&base64)?;
    let key = Key::new(alg, &secret, encoded_key_name, None, None)?;

    info!(
        "Adding TSIG key '{}' ({}) to the key store",
        key.name(),
        key.algorithm()
    );

    Ok(key)
}
