//! Some helper functions for HTTP calls (based on krill::commons::httpclient)
#![deny(dead_code)]
#![deny(unused_variables)]

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use bytes::Bytes;
use clap::crate_version;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE, USER_AGENT};
use reqwest::{Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde::Serialize;

/// The HTTP client request timeout.
const HTTP_CLIENT_TIMEOUT_SECS: u64 = 120;

const JSON_CONTENT: &str = "application/json";

//------------ HTTP Client Functions -----------------------------------------

// /// Gets the Bearer token from the request header, if present.
// pub fn get_bearer_token(
//     request: &hyper::Request<hyper::body::Incoming>,
// ) -> Option<Token> {
//     request
//         .headers()
//         .get(hyper::header::AUTHORIZATION)
//         .and_then(|value| value.to_str().ok())
//         .and_then(|header_string| {
//             header_string
//                 .strip_prefix("Bearer ")
//                 .map(|s| Token::from(s.trim()))
//         })
// }

/// Performs a GET request that expects a json response that can be
/// deserialized into an owned value of the expected type. Returns an
/// error if nothing is returned.
pub async fn get_json<T: DeserializeOwned>(uri: &str) -> Result<T, HttpError> {
    let headers = headers(uri, Some(JSON_CONTENT))?;

    let res = client(uri)?
        .get(uri)
        .headers(headers)
        .send()
        .await
        .map_err(|e| HttpError::execute(uri, e))?;

    process_json_response(uri, res).await
}

/// Performs a get request and expects a response that can be turned
/// into a string (in particular, not a binary response).
pub async fn get_text(uri: &str) -> Result<String, HttpError> {
    let headers = headers(uri, None)?;
    let res = client(uri)?
        .get(uri)
        .headers(headers)
        .send()
        .await
        .map_err(|e| HttpError::execute(uri, e))?;

    text_response(uri, res).await
}

/// Checks that there is a 200 OK response at the given URI. Discards the
/// response body.
pub async fn get_ok(uri: &str) -> Result<(), HttpError> {
    let headers = headers(uri, None)?;
    let res = client(uri)?
        .get(uri)
        .headers(headers)
        .send()
        .await
        .map_err(|e| HttpError::execute(uri, e))?;

    opt_text_response(uri, res).await?; // Will return nice errors with possible body.
    Ok(())
}

/// Performs a POST of data that can be serialized into json, and expects
/// a 200 OK response, without a body.
pub async fn post_json(uri: &str, data: impl Serialize) -> Result<(), HttpError> {
    let body = serde_json::to_string_pretty(&data)
        .map_err(|e| HttpError::request_build_json(uri, e))?;

    let headers = headers(uri, Some(JSON_CONTENT))?;

    let res = client(uri)?
        .post(uri)
        .headers(headers)
        .body(body)
        .send()
        .await
        .map_err(|e| HttpError::execute(uri, e))?;

    empty_response(uri, res).await
}

/// Performs a POST of data that can be serialized into json, and expects
/// a json response that can be deserialized into the an owned value of the
/// expected type.
pub async fn post_json_with_response<T: DeserializeOwned>(
    uri: &str,
    data: impl Serialize,
) -> Result<T, HttpError> {
    match post_json_with_opt_response(uri, data).await? {
        None => Err(HttpError::response(uri, "expected JSON response")),
        Some(res) => Ok(res),
    }
}

/// Performs a POST of data that can be serialized into json, and expects
/// an optional json response that can be deserialized into the an owned
/// value of the expected type.
pub async fn post_json_with_opt_response<T: DeserializeOwned>(
    uri: &str,
    data: impl Serialize,
) -> Result<Option<T>, HttpError> {
    let body = serde_json::to_string_pretty(&data)
        .map_err(|e| HttpError::request_build_json(uri, e))?;

    let headers = headers(uri, Some(JSON_CONTENT))?;
    let res = client(uri)?
        .post(uri)
        .headers(headers)
        .body(body)
        .send()
        .await
        .map_err(|e| HttpError::execute(uri, e))?;

    process_opt_json_response(uri, res).await
}

/// Performs a POST with no data to the given URI and expects and empty 200 OK
/// response.
pub async fn post_empty(uri: &str) -> Result<(), HttpError> {
    let res = do_empty_post(uri).await?;
    empty_response(uri, res).await
}

/// Performs a POST with no data to the given URI and expects a response.
pub async fn post_empty_with_response<T: DeserializeOwned>(
    uri: &str,
) -> Result<T, HttpError> {
    let res = do_empty_post(uri).await?;
    process_json_response(uri, res).await
}

async fn do_empty_post(uri: &str) -> Result<Response, HttpError> {
    let headers = headers(uri, Some(JSON_CONTENT))?;
    client(uri)?
        .post(uri)
        .headers(headers)
        .send()
        .await
        .map_err(|e| HttpError::execute(uri, e))
}

/// Posts binary data, and expects a binary response. Includes the full krill
/// version as the user agent. Intended for sending RFC 6492 (provisioning)
/// and 8181 (publication) to the trusted parent or publication server.
///
/// Note: Bytes may be empty if the post was successful, but the response was
/// empty.
pub async fn post_binary_with_full_ua(
    uri: &str,
    data: &Bytes,
    content_type: &str,
    timeout: u64,
) -> Result<Bytes, HttpError> {
    let body = data.to_vec();

    let mut headers = HeaderMap::new();

    let ua_string = concat!("krill/{}", crate_version!());
    let user_agent_value = HeaderValue::from_str(ua_string)
        .map_err(|e| HttpError::request_build(uri, e))?;
    let content_type_value = HeaderValue::from_str(content_type)
        .map_err(|e| HttpError::request_build(uri, e))?;

    headers.insert(USER_AGENT, user_agent_value);
    headers.insert(CONTENT_TYPE, content_type_value);

    let client = reqwest::ClientBuilder::new()
        .timeout(Duration::from_secs(timeout))
        .build()
        .map_err(|e| HttpError::request_build(uri, e))?;

    let res = client
        .post(uri)
        .headers(headers)
        .body(body)
        .send()
        .await
        .map_err(|e| HttpError::execute(uri, e))?;

    match res.status() {
        StatusCode::OK => {
            let bytes = res.bytes().await.map_err(|e| {
                HttpError::response(uri, format!("cannot get body: {e}"))
            })?;
            Ok(bytes)
        }
        _ => Err(HttpError::from_response(uri, res).await),
    }
}

/// Sends a delete request to the specified url.
pub async fn delete(uri: &str) -> Result<(), HttpError> {
    // report_delete(uri, None);

    let headers = headers(uri, None)?;
    let res = client(uri)?
        .delete(uri)
        .headers(headers)
        .send()
        .await
        .map_err(|e| HttpError::execute(uri, e))?;

    match res.status() {
        StatusCode::OK => Ok(()),
        _ => Err(HttpError::from_response(uri, res).await),
    }
}

// #[allow(clippy::result_large_err)]
// fn load_root_cert(path_str: &str) -> Result<reqwest::Certificate, Error> {
//     let path = PathBuf::from_str(path_str)
//         .map_err(|e| Error::request_build_https_cert(path_str, e))?;
//     let file = file::read(&path)
//         .map_err(|e| Error::request_build_https_cert(path_str, e))?;
//     reqwest::Certificate::from_pem(file.as_ref())
//         .map_err(|e| Error::request_build_https_cert(path_str, e))
// }

/// Default client for Krill use cases.
#[allow(clippy::result_large_err)]
fn client(uri: &str) -> Result<reqwest::Client, HttpError> {
    client_with_tweaks(uri, Duration::from_secs(HTTP_CLIENT_TIMEOUT_SECS), true)
}

/// Client with tweaks - in particular needed by the openid connect client
#[allow(clippy::result_large_err)]
pub fn client_with_tweaks(
    uri: &str,
    timeout: Duration,
    allow_redirects: bool,
) -> Result<reqwest::Client, HttpError> {
    let mut builder = reqwest::ClientBuilder::new()
        .timeout(timeout)
        .http2_prior_knowledge();

    if !allow_redirects {
        builder = builder.redirect(reqwest::redirect::Policy::none());
    }

    // if let Ok(cert_list) = env::var(KRILL_HTTPS_ROOT_CERTS_ENV) {
    //     for path in cert_list.split(':') {
    //         let cert = load_root_cert(path)?;
    //         builder = builder.add_root_certificate(cert);
    //     }
    // }

    // if uri.starts_with("https://localhost")
    //     || uri.starts_with("https://127.0.0.1")
    // {
    //     builder.danger_accept_invalid_certs(true).build()
    // } else {
    builder
        .build()
        // }
        .map_err(|e| HttpError::request_build(uri, e))
}

#[allow(clippy::result_large_err)]
fn headers(uri: &str, content_type: Option<&str>) -> Result<HeaderMap, HttpError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        hyper::header::USER_AGENT,
        HeaderValue::from_static(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION"),
        )),
    );

    if let Some(content_type) = content_type {
        headers.insert(
            hyper::header::CONTENT_TYPE,
            HeaderValue::from_str(content_type)
                .map_err(|e| HttpError::request_build(uri, e))?,
        );
    }
    // if let Some(token) = token {
    //     headers.insert(
    //         hyper::header::AUTHORIZATION,
    //         HeaderValue::from_str(&format!("Bearer {token}"))
    //             .map_err(|e| Error::request_build(uri, e))?,
    //     );
    // }
    Ok(headers)
}

async fn process_json_response<T: DeserializeOwned>(
    uri: &str,
    res: Response,
) -> Result<T, HttpError> {
    match process_opt_json_response(uri, res).await? {
        None => Err(HttpError::response(uri, "got empty response body")),
        Some(res) => Ok(res),
    }
}

async fn process_opt_json_response<T: DeserializeOwned>(
    uri: &str,
    res: Response,
) -> Result<Option<T>, HttpError> {
    match opt_text_response(uri, res).await? {
        None => Ok(None),
        Some(s) => {
            let res: T = serde_json::from_str(&s).map_err(|e| {
                HttpError::response(
                    uri,
                    format!("could not parse JSON response: {e}"),
                )
            })?;
            Ok(Some(res))
        }
    }
}

async fn empty_response(uri: &str, res: Response) -> Result<(), HttpError> {
    match opt_text_response(uri, res).await? {
        None => Ok(()),
        Some(_) => Err(HttpError::response(uri, "expected empty response")),
    }
}

async fn text_response(uri: &str, res: Response) -> Result<String, HttpError> {
    match opt_text_response(uri, res).await? {
        None => Err(HttpError::response(uri, "expected response body")),
        Some(s) => Ok(s),
    }
}

async fn opt_text_response(
    uri: &str,
    res: Response,
) -> Result<Option<String>, HttpError> {
    match res.status() {
        StatusCode::OK => match res.text().await.ok() {
            None => Ok(None),
            Some(s) => {
                if s.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(s))
                }
            }
        },
        StatusCode::FORBIDDEN => Err(HttpError::Forbidden(uri.to_string())),
        _ => Err(HttpError::from_response(uri, res).await),
    }
}

//------------ Error ---------------------------------------------------------

type ErrorUri = String;
type RootCertPath = String;
type ErrorMessage = String;

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum HttpError {
    RequestBuild(ErrorUri, ErrorMessage),
    RequestBuildHttpsCert(RootCertPath, ErrorMessage),

    RequestExecute(ErrorUri, ErrorMessage),

    Response(ErrorUri, ErrorMessage),
    Forbidden(ErrorUri),
    ErrorResponse(ErrorUri, StatusCode),
    ErrorResponseWithBody(ErrorUri, StatusCode, String),
    ErrorResponseWithJson(ErrorUri, StatusCode, Box<ApiErrorResponse>),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            HttpError::RequestBuild(uri, msg) => write!(
                f,
                "Issue creating request for URI: {uri}, error: {msg}"
            ),
            HttpError::RequestBuildHttpsCert(path, msg) => {
                write!(
                    f,
                    "Cannot use configured HTTPS root cert '{path}'. Error: {msg}"
                )
            }

            HttpError::RequestExecute(uri, msg) => {
                write!(f, "Issue accessing URI: {uri}, error: {msg}")
            }

            HttpError::Response(uri, msg) => write!(
                f,
                "Issue processing response from URI: {uri}, error: {msg}"
            ),
            HttpError::Forbidden(uri) => {
                write!(f, "Got 'Forbidden' response for URI: {uri}")
            }
            HttpError::ErrorResponse(uri, code) => {
                write!(
                    f,
                    "Issue processing response from URI: {uri}, \
                     error: unexpected status code {code}"
                )
            }
            HttpError::ErrorResponseWithBody(uri, code, e) => {
                write!(
                    f,
                    "Error response from URI: {uri}, Status: {code}, Error: {e}"
                )
            }
            HttpError::ErrorResponseWithJson(uri, code, res) => write!(
                f,
                "Error response from URI: {uri}, Status: {code}, ErrorResponse: {res}"
            ),
        }
    }
}

impl HttpError {
    pub fn request_build(uri: &str, msg: impl fmt::Display) -> Self {
        HttpError::RequestBuild(uri.to_string(), msg.to_string())
    }

    pub fn request_build_json(uri: &str, e: impl fmt::Display) -> Self {
        HttpError::RequestBuild(
            uri.to_string(),
            format!("could not serialize type to JSON: {e}"),
        )
    }

    pub fn request_build_https_cert(
        path: &str,
        msg: impl fmt::Display,
    ) -> Self {
        HttpError::RequestBuildHttpsCert(path.to_string(), msg.to_string())
    }

    pub fn execute(uri: &str, msg: impl fmt::Display) -> Self {
        HttpError::RequestExecute(uri.to_string(), msg.to_string())
    }

    pub fn response(uri: &str, msg: impl fmt::Display) -> Self {
        HttpError::Response(uri.to_string(), msg.to_string())
    }

    pub fn forbidden(uri: &str) -> Self {
        HttpError::Forbidden(uri.to_string())
    }

    pub fn response_unexpected_status(uri: &str, status: StatusCode) -> Self {
        HttpError::ErrorResponse(uri.to_string(), status)
    }

    async fn from_response(uri: &str, res: Response) -> HttpError {
        let status = res.status();
        match res.text().await {
            Ok(body) => {
                if body.is_empty() {
                    Self::response_unexpected_status(uri, status)
                } else {
                    match serde_json::from_str::<ApiErrorResponse>(&body) {
                        Ok(res) => HttpError::ErrorResponseWithJson(
                            uri.to_string(),
                            status,
                            Box::new(res),
                        ),
                        Err(_) => HttpError::ErrorResponseWithBody(
                            uri.to_string(),
                            status,
                            body,
                        ),
                    }
                }
            }
            _ => Self::response_unexpected_status(uri, status),
        }
    }

    /// Returns the HTTP status code if we got that far.
    ///
    /// Returns `None` if any other kind of error happened.
    pub fn status_code(&self) -> Option<StatusCode> {
        match self {
            Self::ErrorResponse(_, code)
            | Self::ErrorResponseWithBody(_, code, _)
            | Self::ErrorResponseWithJson(_, code, _) => Some(*code),
            _ => None,
        }
    }
}

//------------ ApiErrorResponse ----------------------------------------------

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ApiErrorResponse {
    /// The error label.
    pub label: String,

    /// The error message.
    pub msg: String,

    /// Arguments with details about the error.
    pub args: HashMap<String, String>,
}

impl ApiErrorResponse {
    pub fn new(label: &str, msg: impl fmt::Display) -> Self {
        ApiErrorResponse {
            label: label.to_string(),
            msg: msg.to_string(),
            args: HashMap::new(),
        }
    }

    pub fn with_arg(mut self, key: &str, value: impl fmt::Display) -> Self {
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

impl fmt::Display for ApiErrorResponse {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", &serde_json::to_string(&self).unwrap())
    }
}
