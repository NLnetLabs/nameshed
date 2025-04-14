//! Error reporting.

use std::collections::HashMap;
use std::fmt;

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

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
        &self,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut serializer = serializer.serialize_struct("Success", 1)?;
        serializer.serialize_field("status", "Ok")?;
        serializer.end()
    }
}

//------------ ErrorResponse -------------------------------------------------

/// An API error response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ErrorResponse {
    /// The error label.
    pub label: String,

    /// The error message.
    pub msg: String,

    /// Arguments with details about the error.
    pub args: HashMap<String, String>,
}

impl ErrorResponse {
    pub fn new(label: &str, msg: impl fmt::Display) -> Self {
        ErrorResponse {
            label: label.to_string(),
            msg: msg.to_string(),
            args: HashMap::new(),
        }
    }

    fn with_arg(mut self, key: &str, value: impl fmt::Display) -> Self {
        self.args.insert(key.to_string(), value.to_string());
        self
    }

    pub fn with_cause(self, cause: impl fmt::Display) -> Self {
        self.with_arg("cause", cause)
    }

    pub fn with_uri(self, uri: impl fmt::Display) -> Self {
        self.with_arg("uri", uri)
    }

    pub fn with_base_uri(self, base_uri: impl fmt::Display) -> Self {
        self.with_arg("base_uri", base_uri)
    }
}

impl fmt::Display for ErrorResponse {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", &serde_json::to_string(&self).unwrap())
    }
}
