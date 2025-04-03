pub mod status;

use std::fmt;

use hyper::{Body, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};

use crate::http::PercentDecodedPath;

// MUST include leading and trailing slash ("/")
pub const API_BASE_PATH: &str = "/api/";

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
        {$else:expr, $($x:expr => $y:expr),* $(,)?} => {
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
            bad_request(),
            api_path.eq("login") => login(),
            api_path.starts_with("users/") => users(),
            api_path.starts_with("stuff/") => stuff(),
        }
        // if api_path.eq("login") {
        //     Response::builder()
        //         .status(StatusCode::OK)
        //         .header("Content-Type", "text/plain")
        //         .body(Body::from(format!("You are now logged in. Thanks")))
        //         .unwrap()
        // } else if api_path.starts_with("users/") {
        //     todo!()
        // } else if api_path.starts_with("stuff/") {
        //     Response::builder()
        //         .status(StatusCode::OK)
        //         .header("Content-Type", "text/plain")
        //         .body(Body::from(format!(
        //             "it works: {api_path}\n{}",
        //             get_body_text(request).await.unwrap()
        //         )))
        //         .unwrap()
        // } else {
        //     bad_request()
        // }
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

/// Map the API request to its version's API handler
pub async fn handle_api_request(request: Request<Body>) -> Response<Body> {
    let req_path = request.uri().decoded_path().into_owned();
    if req_path.starts_with(API_BASE_PATH) {
        let api_path = req_path.strip_prefix(API_BASE_PATH).unwrap();
        if api_path.starts_with("v1/") {
            v1::handle_api_request(
                request,
                api_path.strip_prefix("v1/").unwrap(),
            )
            .await
        } else {
            bad_request()
        }
    } else {
        bad_request()
    }
}

pub async fn get_body_text(req: Request<Body>) -> Option<String> {
    let full_body = hyper::body::to_bytes(req.into_body()).await.unwrap();
    Some(String::from_utf8_lossy(&full_body).to_string())
}

fn bad_request() -> Response<Body> {
    Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .header("Content-Type", "text/plain")
        .body("Bad Request".into())
        .unwrap()
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
