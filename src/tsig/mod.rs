//! Managing TSIG keys.

use domain::tsig;

/// A store of TSIG keys.
#[derive(Debug)]
pub struct TsigStore {
    /// A map of known TSIG keys by name.
    pub map: foldhash::HashMap<tsig::KeyName, tsig::Key>,
}
