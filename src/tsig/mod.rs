//! Managing TSIG keys.

use std::{collections::hash_map, fmt, io, sync::Arc, time::Duration};

use domain::tsig;

use crate::{center::Center, config::Config, zone::ZoneByPtr};

pub mod file;

//----------- TsigStore --------------------------------------------------------

/// A store of TSIG keys.
#[derive(Debug, Default)]
pub struct TsigStore {
    /// A map of known TSIG keys by name.
    pub map: foldhash::HashMap<tsig::KeyName, TsigKey>,

    /// An enqueued save of this state.
    ///
    /// The enqueued save operation will persist the current state in a short
    /// duration of time.  If the field is `None`, and the state is changed, a
    /// new save operation should be enqueued.
    pub enqueued_save: Option<tokio::task::JoinHandle<()>>,
}

impl TsigStore {
    /// Construct a new [`TsigStore`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Initialize the store from its file.
    pub fn init_from_file(&mut self, config: &Config) -> io::Result<()> {
        file::Spec::load(&config.tsig_store_path)?.parse(self);
        Ok(())
    }

    /// Mark the store as dirty.
    ///
    /// A persistence operation for the store will be enqueued (unless one
    /// already exists), so that it will be saved in the near future.
    pub fn mark_dirty(&mut self, center: &Arc<Center>) {
        if self.enqueued_save.is_some() {
            // A save is already enqueued; nothing to do.
            return;
        }

        // Enqueue a new save.
        let center = center.clone();
        let task = tokio::spawn(async move {
            // TODO: Make this time configurable.
            tokio::time::sleep(Duration::from_secs(5)).await;

            let (path, spec);
            {
                // Load the global state.
                let mut state = center.state.lock().unwrap();
                let Some(_) = state
                    .tsig_store
                    .enqueued_save
                    .take_if(|s| s.id() == tokio::task::id())
                else {
                    // 'enqueued_save' does not match what we set, so somebody
                    // else set it to 'None' first.  Don't do anything.
                    log::trace!("Ignoring enqueued save due to race");
                    return;
                };

                path = state.config.tsig_store_path.clone();
                spec = file::Spec::build(&state.tsig_store);
            }

            // Save the TSIG store.
            match spec.save(&path) {
                Ok(()) => log::debug!("Saved the TSIG store (to '{path}')"),
                Err(err) => {
                    log::error!("Could not save the TSIG store to '{path}': {err}");
                }
            }
        });
        self.enqueued_save = Some(task);
    }
}

//----------- Actions ----------------------------------------------------------

/// Persist the store immediately.
pub fn save_now(center: &Center) {
    let (path, spec);
    {
        // Load the global state.
        let mut state = center.state.lock().unwrap();

        // If there was an enqueued save operation, stop it.
        if let Some(save) = state.tsig_store.enqueued_save.take() {
            save.abort();
        }

        path = state.config.tsig_store_path.clone();
        spec = file::Spec::build(&state.tsig_store);
    }

    // Save the TSIG store.
    match spec.save(&path) {
        Ok(()) => log::debug!("Saved the TSIG store (to '{path}')"),
        Err(err) => {
            log::error!("Could not save the TSIG store to '{path}': {err}");
        }
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
pub fn reload(center: &Arc<Center>) {
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
    state.tsig_store.mark_dirty(center);
}

/// Import a TSIG key.
pub fn import_key(
    center: &Arc<Center>,
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
        }
        hash_map::Entry::Vacant(entry) => {
            entry.insert(TsigKey {
                inner: Arc::new(key),
                material: material.into(),
                zones: Default::default(),
            });
        }
    }
    state.tsig_store.mark_dirty(center);
    Ok(())
}

/// Generate a TSIG key.
pub fn generate_key(
    center: &Arc<Center>,
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
        }
        hash_map::Entry::Vacant(entry) => {
            entry.insert(TsigKey {
                inner: Arc::new(key),
                material: (*material).into(),
                zones: Default::default(),
            });
        }
    }
    state.tsig_store.mark_dirty(center);
    Ok(())
}

/// Remove a TSIG key.
pub fn remove_key(center: &Arc<Center>, name: &tsig::KeyName) -> Result<(), RemoveError> {
    // Lock the global state and try to remove the key.
    let mut state = center.state.lock().unwrap();
    match state.tsig_store.map.entry(name.clone()) {
        hash_map::Entry::Occupied(entry) => {
            if !entry.get().zones.is_empty() {
                return Err(RemoveError::Used);
            }
            entry.remove_entry();
        }
        hash_map::Entry::Vacant(_) => return Err(RemoveError::NotFound),
    }
    state.tsig_store.mark_dirty(center);
    Ok(())
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
