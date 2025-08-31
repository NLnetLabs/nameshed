//! Managing TSIG keys.

use std::{collections::hash_map, fmt, fs, io, sync::Arc};

use domain::tsig;

use crate::{center::Center, config::Config, zone::ZoneByPtr};

pub mod file;

//----------- TsigStore --------------------------------------------------------

/// A store of TSIG keys.
#[derive(Debug, Default)]
pub struct TsigStore {
    /// A map of known TSIG keys by name.
    pub map: foldhash::HashMap<tsig::KeyName, TsigKey>,
}

impl TsigStore {
    /// Construct a new [`TsigStore`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Load the store.
    pub fn load(&mut self, config: &Config) -> io::Result<()> {
        file::Spec::load(&config.tsig_store_path)?.parse(self);
        Ok(())
    }

    /// Save the store.
    pub fn save(&self, config: &Config) -> io::Result<()> {
        fs::create_dir_all(
            config
                .tsig_store_path
                .parent()
                .ok_or(io::ErrorKind::IsADirectory)?,
        )?;

        file::Spec::build(self).save(&config.tsig_store_path)
    }
}

//----------- TsigKey ----------------------------------------------------------

/// A TSIG key.
pub struct TsigKey {
    /// The underlying key.
    pub inner: Arc<tsig::Key>,

    /// The secret key material.
    material: Box<[u8]>,

    /// The set of zones depending on this key.
    pub zones: foldhash::HashSet<ZoneByPtr>,
}

impl fmt::Debug for TsigKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct Zones<'a>(&'a TsigKey);

        impl fmt::Debug for Zones<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_set()
                    .entries(self.0.zones.iter().map(|z| &z.0.name))
                    .finish()
            }
        }

        f.debug_struct("TsigKey")
            .field("inner", &self.inner)
            // Don't print the secret key material.
            .field("material", &"<secret>")
            // Only print the name of each zone.
            .field("zones", &Zones(self))
            .finish()
    }
}

//----------- Actions ----------------------------------------------------------

/// Reload the TSIG store.
pub fn reload(center: &Center) {
    let path = {
        let state = center.state.lock().unwrap();
        state.config.tsig_store_path.clone()
    };

    let spec = match file::Spec::load(&path) {
        Ok(spec) => spec,
        Err(err) => {
            log::error!("Could not reload the TSIG store: {err}");
            return;
        }
    };

    let mut state = center.state.lock().unwrap();
    spec.parse(&mut state.tsig_store);
}

/// Import a TSIG key.
pub fn import_key(
    center: &Center,
    name: tsig::KeyName,
    algorithm: tsig::Algorithm,
    material: &[u8],
    replace: bool,
) -> Result<(), ImportError> {
    // Prepare the key.
    let key = match tsig::Key::new(algorithm, material, name.clone(), None, None) {
        Ok(key) => key,
        Err(tsig::NewKeyError::BadMinMacLen) => unreachable!(),
        Err(tsig::NewKeyError::BadSigningLen) => unreachable!(),
    };

    // Lock the global state and insert the new key.
    //
    // NOTE: 'HashMap::insert()' overwrites the existing entry.
    let mut state = center.state.lock().unwrap();
    match state.tsig_store.map.entry(name) {
        hash_map::Entry::Occupied(mut entry) => {
            if !replace {
                return Err(ImportError::AlreadyExists);
            }

            let state = entry.get_mut();
            state.inner = Arc::new(key);
            state.material = material.into();
            Ok(())
        }
        hash_map::Entry::Vacant(entry) => {
            entry.insert(TsigKey {
                inner: Arc::new(key),
                material: material.into(),
                zones: Default::default(),
            });
            Ok(())
        }
    }
}

/// Generate a TSIG key.
pub fn generate_key(
    center: &Center,
    name: tsig::KeyName,
    algorithm: tsig::Algorithm,
    replace: bool,
) -> Result<(), GenerateError> {
    // Prepare the key.
    let rng = ring::rand::SystemRandom::new();
    let (key, material) = match tsig::Key::generate(algorithm, &rng, name.clone(), None, None) {
        Ok((key, material)) => (key, material),
        Err(tsig::GenerateKeyError::BadMinMacLen) => unreachable!(),
        Err(tsig::GenerateKeyError::BadSigningLen) => unreachable!(),
        Err(tsig::GenerateKeyError::GenerationFailed) => return Err(GenerateError::Implementation),
    };

    // Lock the global state and insert the new key.
    //
    // NOTE: 'HashMap::insert()' overwrites the existing entry.
    let mut state = center.state.lock().unwrap();
    match state.tsig_store.map.entry(name) {
        hash_map::Entry::Occupied(mut entry) => {
            if !replace {
                return Err(GenerateError::AlreadyExists);
            }

            let state = entry.get_mut();
            state.inner = Arc::new(key);
            state.material = (*material).into();
            Ok(())
        }
        hash_map::Entry::Vacant(entry) => {
            entry.insert(TsigKey {
                inner: Arc::new(key),
                material: (*material).into(),
                zones: Default::default(),
            });
            Ok(())
        }
    }
}

/// Remove a TSIG key.
pub fn remove_key(center: &Center, name: &tsig::KeyName) -> Result<(), RemoveError> {
    // Lock the global state and try to remove the key.
    let mut state = center.state.lock().unwrap();
    match state.tsig_store.map.entry(name.clone()) {
        hash_map::Entry::Occupied(entry) => {
            if !entry.get().zones.is_empty() {
                return Err(RemoveError::Used);
            }
            entry.remove_entry();
            Ok(())
        }
        hash_map::Entry::Vacant(_) => Err(RemoveError::NotFound),
    }
}

//----------- ImportError ------------------------------------------------------

/// An error importing a TSIG key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportError {
    /// A key of the same name already exists.
    AlreadyExists,
}

impl std::error::Error for ImportError {}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::AlreadyExists => write!(f, "a key of the same name already exists"),
        }
    }
}

//----------- GenerateError ----------------------------------------------------

/// An error generating a TSIG key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GenerateError {
    /// A key of the same name already exists.
    AlreadyExists,

    /// An implementation error occurred.
    Implementation,
}

impl std::error::Error for GenerateError {}

impl fmt::Display for GenerateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GenerateError::AlreadyExists => write!(f, "a key of the same name already exists"),
            GenerateError::Implementation => write!(f, "an implementation error occurred"),
        }
    }
}

//----------- RemoveError ------------------------------------------------------

/// An error removing a TSIG key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoveError {
    /// No such key exists.
    NotFound,

    /// The key is in use.
    Used,
}

impl std::error::Error for RemoveError {}

impl fmt::Display for RemoveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RemoveError::NotFound => write!(f, "could not find the requested key"),
            RemoveError::Used => write!(f, "the key is currently in use"),
        }
    }
}
