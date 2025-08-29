//! Managing TSIG keys.

use std::{fmt, fs, io, sync::Arc};

use domain::tsig;

use crate::config::Config;

pub mod file;

//----------- TsigStore --------------------------------------------------------

/// A store of TSIG keys.
#[derive(Clone, Debug, Default)]
pub struct TsigStore {
    /// A map of known TSIG keys by name.
    pub map: foldhash::HashMap<tsig::KeyName, Arc<TsigKey>>,
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
    /// The underlying key type.
    pub inner: tsig::Key,

    /// The secret key material.
    material: Box<[u8]>,
}

impl fmt::Debug for TsigKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Don't debug the secret key material
        self.inner.fmt(f)
    }
}
