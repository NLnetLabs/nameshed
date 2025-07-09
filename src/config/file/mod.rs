//! The configuration file.

use std::{fmt, sync::Arc};

use camino::Utf8Path;
use serde::Deserialize;

use super::{Config, Setting};

pub mod v1;

//----------- FileSpec ---------------------------------------------------------

/// A configuration file.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "version")]
pub enum FileSpec {
    /// The version 1 format.
    V1(v1::Spec),
}

//--- Processing

impl FileSpec {
    /// Load the configuration file.
    pub fn load(path: &Utf8Path) -> Result<Self, FileError> {
        let text = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }

    /// Build the Nameshed configuration.
    pub fn build(self, config_file: Setting<Box<Utf8Path>>) -> Config {
        match self {
            Self::V1(spec) => spec.build(config_file),
        }
    }
}

//----------- FileError --------------------------------------------------------

/// An error in processing Nameshed's configuration file.
#[derive(Clone, Debug)]
pub enum FileError {
    /// The file could not be loaded.
    Load(Arc<std::io::Error>),

    /// The file could not be parsed.
    Parse(toml::de::Error),
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileError::Load(error) => error.fmt(f),
            FileError::Parse(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for FileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Load(error) => Some(error),
            Self::Parse(error) => Some(error),
        }
    }
}

impl PartialEq for FileError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Load(l), Self::Load(r)) => l.kind() == r.kind(),
            (Self::Parse(l), Self::Parse(r)) => l == r,
            _ => false,
        }
    }
}

impl Eq for FileError {}

impl From<std::io::Error> for FileError {
    fn from(value: std::io::Error) -> Self {
        Self::Load(Arc::new(value))
    }
}

impl From<toml::de::Error> for FileError {
    fn from(value: toml::de::Error) -> Self {
        Self::Parse(value)
    }
}
