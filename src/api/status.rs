//! Error reporting.

use std::fmt;
use serde::{Serialize, Serializer};
use serde::ser::SerializeStruct;


//------------ Success -------------------------------------------------------

/// An empty, successful API response.
///
/// This type needs to be used instead of `()` to make conversion into
/// [`Report`][crate::cli::report::Report] work.
#[derive(Clone, Copy, Debug)]
pub struct Success;

impl fmt::Display for Success {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("Ok")
    }
}

impl Serialize for Success {
    fn serialize<S: Serializer>(
        &self, serializer: S
    ) -> Result<S::Ok, S::Error> {
        let mut serializer = serializer.serialize_struct("Success", 1)?;
        serializer.serialize_field("status", "Ok")?;
        serializer.end()
    }
}
