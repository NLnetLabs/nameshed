pub mod status;

use std::fmt;

use hyper::{Body, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};

use crate::http::PercentDecodedPath;

pub const API_BASE_PATH: &str = "/api/";

// mod v1 { }

pub async fn handle_api_request(request: Request<Body>) -> Response<Body> {
    let req_path = request.uri().decoded_path().into_owned();
    if req_path.starts_with(API_BASE_PATH) {
        let api_path = req_path.strip_prefix(API_BASE_PATH).unwrap_or_default();
        let response = Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/plain")
            .body(Body::from(format!(
                "it works: {api_path}\n{}",
                get_body_text(request).await.unwrap()
            )))
            .unwrap();

        response
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
