use hyper::{Body, Request, Response, StatusCode};

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
