//! The TSIG keys file.

use std::{
    fs::File,
    io::{self, BufReader},
};

use camino::Utf8Path;
use serde::{Deserialize, Serialize};

use crate::tsig::TsigStore;

pub mod v1;

//----------- Spec -------------------------------------------------------------

/// A TSIG keys file.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "version")]
pub enum Spec {
    /// The version 1 format.
    V1(v1::Spec),
}

//--- Conversion

impl Spec {
    /// Parse from this specification.
    pub fn parse(self, store: &mut TsigStore) {
        match self {
            Self::V1(spec) => spec.parse(store),
        }
    }

    /// Build into this specification.
    pub fn build(store: &TsigStore) -> Self {
        Self::V1(v1::Spec::build(store))
    }
}

//--- Loading / saving

impl Spec {
    /// Load and parse this specification from a file.
    pub fn load(path: &Utf8Path) -> io::Result<Self> {
        let file = BufReader::new(File::open(path)?);
        let spec = serde_json::from_reader(file)?;
        Ok(spec)
    }

    /// Build and save this specification to a file.
    pub fn save(&self, path: &Utf8Path) -> io::Result<()> {
        let text = serde_json::to_string(self)?;
        crate::util::write_file(path, text.as_bytes())
    }
}
