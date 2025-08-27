//! Managing TSIG keys.

use std::{fmt, io};

use domain::tsig;

use crate::config::Config;

pub mod file;

//----------- TsigStore --------------------------------------------------------

/// A store of TSIG keys.
#[derive(Clone, Debug, Default)]
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
        // TODO: Pick a path from the config.
        let _ = config;
        let path = "/var/db/nameshed/tsig-keys";

        file::Spec::load(path.as_ref())?.parse(self);
        Ok(())
    }

    /// Save the store.
    pub fn save(&self, config: &Config) -> io::Result<()> {
        // TODO: Pick a path from the config.
        let _ = config;
        let path = "/var/db/nameshed/tsig-keys";

        file::Spec::build(self).save(path.as_ref())
    }
}

//----------- TsigKey ----------------------------------------------------------

/// A TSIG key.
#[derive(Clone)]
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
