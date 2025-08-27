use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use domain::base::ToName;
use domain::tsig::{Algorithm, Key, KeyName, KeyStore};

#[allow(dead_code)]
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
