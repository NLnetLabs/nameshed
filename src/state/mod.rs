//! Serializing global state.

use std::{
    fs,
    io::{self, BufReader},
};

use camino::Utf8Path;
use serde::{Deserialize, Serialize};

use crate::center::{Center, Change, State};

pub mod v1;

//----------- Actions ----------------------------------------------------------

/// Persist the global state immediately.
pub fn save_now(center: &Center) {
    let (path, spec);
    {
        // Load the global state.
        let mut state = center.state.lock().unwrap();

        // If there was an enqueued save operation, stop it.
        if let Some(save) = state.enqueued_save.take() {
            save.abort();
        }

        path = state.config.daemon.state_file.value().clone();
        spec = Spec::build(&state);
    }

    // Save the global state.
    match spec.save(&path) {
        Ok(()) => log::debug!("Saved the global state (to '{path}')"),
        Err(err) => {
            log::error!("Could not save the global state to '{path}': {err}");
        }
    }
}

//----------- StateSpec --------------------------------------------------------

/// A state file.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "version")]
pub enum Spec {
    /// The version 1 format.
    V1(v1::Spec),
}

//--- Conversion

impl Spec {
    /// Parse from this specification.
    pub fn parse_into(self, state: &mut State, changes: &mut Vec<Change>) {
        match self {
            Self::V1(spec) => spec.parse_into(state, changes),
        }
    }

    /// Build into this specification.
    pub fn build(state: &State) -> Self {
        Self::V1(v1::Spec::build(state))
    }
}

//--- Loading / Saving

impl Spec {
    /// Load and parse this specification from a file.
    pub fn load(path: &Utf8Path) -> io::Result<Self> {
        let file = BufReader::new(fs::File::open(path)?);
        serde_json::from_reader(file).map_err(|err| err.into())
    }

    /// Build and save this specification to a file.
    pub fn save(&self, path: &Utf8Path) -> io::Result<()> {
        if path.parent().is_none() {
            return Err(io::ErrorKind::IsADirectory.into());
        }

        let text = serde_json::to_string(self)?;
        crate::util::write_file(path, text.as_bytes())
    }
}
