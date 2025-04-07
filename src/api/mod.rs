pub mod status;

use std::fmt;

use hyper::{Body, Request, Response, StatusCode};
use log::{debug, error, info, trace};
use serde::{Deserialize, Serialize};

use crate::http::PercentDecodedPath;

// MUST include leading and trailing slash ("/")
pub const API_BASE_PATH: &str = "/api/";

/// Map the API request to its version's API handler
pub async fn handle_api_request(request: Request<Body>) -> Response<Body> {
    let req_path = request.uri().decoded_path().into_owned();
    if req_path.starts_with(API_BASE_PATH) {
        let api_path = req_path.strip_prefix(API_BASE_PATH).unwrap();
        if let Some(p) = api_path.strip_prefix("v1/") {
            v1::handle_api_request(request, p).await
        } else {
            bad_request()
        }
    } else {
        bad_request()
    }
}

pub async fn consume_body_text(req: Request<Body>) -> Option<String> {
    let full_body = hyper::body::to_bytes(req.into_body()).await;
    match full_body {
        Ok(b) => Some(String::from_utf8_lossy(&b).to_string()),
        Err(_) => {
            error!("Failed to collect HTTP request body");
            None
        }
    }
}

fn bad_request() -> Response<Body> {
    Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .header("Content-Type", "text/plain")
        .body("Bad Request".into())
        .unwrap()
}

//------------ API v1 --------------------------------------------------------

mod v1 {
    use super::*;

    /// A macro to compress the if-else-chain into a more easily copyable and
    /// readable chain of API mappings.
    ///
    /// The first argument must be the fallback else case when no condition matches.
    /// The following arguments are a comma separated list of condition-expression-mappings:
    /// ```ignore
    /// m! {
    ///     else_function_when_no_condition_matches(),
    ///     condition1 => function_call(),
    ///     condition2 => function_call2(),
    /// }
    /// ```
    /// A trailing comma is allowed.
    macro_rules! m {
        {$($x:expr => $y:expr),*, _ => $else:expr $(,)?} => {
            $(
                if $x {
                    $y
                } else
            )* {
                $else
            }
        }
    }

    /// Handle the API request
    ///
    /// The api_path parameter needs to be stripped of any API version prefixes and only contain
    /// the actual API path
    pub async fn handle_api_request(
        request: Request<Body>,
        api_path: &str,
    ) -> Response<Body> {
        m! {
            api_path.eq("health") => health(),
            api_path.eq("login") => login(),
            api_path.starts_with("users/") => users(),
            api_path.starts_with("stuff/") => stuff(),
            _ => bad_request()
        }
    }

    fn login() -> Response<Body> {
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/plain")
            .body(Body::from(format!("You are now logged in. Thanks")))
            .unwrap()
    }

    fn users() -> Response<Body> {
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/plain")
            .body(Body::from(format!("Users\n")))
            .unwrap()
    }

    fn stuff() -> Response<Body> {
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/plain")
            .body(Body::from(format!("Stuff\n")))
            .unwrap()
    }
}

//------------ AuthToken -----------------------------------------------------

/// An authentication token.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct AuthToken(String);

impl From<&str> for AuthToken {
    fn from(s: &str) -> Self {
        AuthToken(s.to_string())
    }
}

impl From<String> for AuthToken {
    fn from(s: String) -> Self {
        AuthToken(s)
    }
}

impl AsRef<str> for AuthToken {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AuthToken {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}
