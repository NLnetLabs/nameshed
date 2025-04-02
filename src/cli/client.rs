//! A client to talk to the nameshed server.

use std::borrow::Cow;

use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use url::Url;

use crate::api::AuthToken;

//------------ NameshedClient ------------------------------------------------

/// A client to talk to a nameshed server.
#[derive(Clone, Debug)]
pub struct NameshedClient {
    /// The base URI of the API server.
    base_uri: Url,

    /// The access token for the API.
    token: AuthToken,
}

/// # Low-level commands
impl NameshedClient {
    /// Creates a cient from a URI and token.
    pub fn new(base_uri: Url, token: AuthToken) -> Self {
        Self { base_uri, token }
    }

    /// Returns the base URI of the server.
    pub fn base_uri(&self) -> &Url {
        &self.base_uri
    }

    /// Returns the access token.
    pub fn token(&self) -> &AuthToken {
        &self.token
    }

    /// Sets the access token.
    pub fn set_token(&mut self, token: AuthToken) {
        self.token = token
    }

    // /// Performs a GET request and checks that it gets a 200 OK back.
    // pub async fn get_ok<'a>(
    //     &self,
    //     path: impl IntoIterator<Item = Cow<'a, str>>,
    // ) -> Result<Success, Error> {
    //     httpclient::get_ok(&self.create_uri(path), Some(&self.token))
    //         .await
    //         .map(|_| Success)
    // }

    // /// Performs a GET request expecting a JSON response.
    // pub async fn get_json<'a, T: DeserializeOwned>(
    //     &self,
    //     path: impl IntoIterator<Item = Cow<'a, str>>,
    // ) -> Result<T, Error> {
    //     httpclient::get_json(&self.create_uri(path), Some(&self.token)).await
    // }

    // /// Performs an empty POST request.
    // pub async fn post_empty<'a>(
    //     &self,
    //     path: impl IntoIterator<Item = Cow<'a, str>>,
    // ) -> Result<Success, Error> {
    //     httpclient::post_empty(&self.create_uri(path), Some(&self.token))
    //         .await
    //         .map(|_| Success)
    // }

    // /// Posts JSON-encoded data and expects a JSON-encoded response.
    // pub async fn post_empty_with_response<'a, T: DeserializeOwned>(
    //     &self,
    //     path: impl IntoIterator<Item = Cow<'a, str>>,
    // ) -> Result<T, Error> {
    //     httpclient::post_empty_with_response(
    //         &self.create_uri(path),
    //         Some(&self.token),
    //     )
    //     .await
    // }

    // /// Posts JSON-encoded data.
    // pub async fn post_json<'a>(
    //     &self,
    //     path: impl IntoIterator<Item = Cow<'a, str>>,
    //     data: impl Serialize,
    // ) -> Result<Success, Error> {
    //     httpclient::post_json(&self.create_uri(path), data, Some(&self.token))
    //         .await
    //         .map(|_| Success)
    // }

    // /// Posts JSON-encoded data and expects a JSON-encoded response.
    // pub async fn post_json_with_response<'a, T: DeserializeOwned>(
    //     &self,
    //     path: impl IntoIterator<Item = Cow<'a, str>>,
    //     data: impl Serialize,
    // ) -> Result<T, Error> {
    //     httpclient::post_json_with_response(
    //         &self.create_uri(path),
    //         data,
    //         Some(&self.token),
    //     )
    //     .await
    // }

    // /// Posts JSON-encoded data and expects an optional JSON-encoded response.
    // pub async fn post_json_with_opt_response<'a, T: DeserializeOwned>(
    //     &self,
    //     path: impl IntoIterator<Item = Cow<'a, str>>,
    //     data: impl Serialize,
    // ) -> Result<Option<T>, Error> {
    //     httpclient::post_json_with_opt_response(
    //         &self.create_uri(path),
    //         data,
    //         Some(&self.token),
    //     )
    //     .await
    // }

    // /// Sends a DELETE request.
    // pub async fn delete<'a>(
    //     &self,
    //     path: impl IntoIterator<Item = Cow<'a, str>>,
    // ) -> Result<Success, Error> {
    //     httpclient::delete(&self.create_uri(path), Some(&self.token))
    //         .await
    //         .map(|_| Success)
    // }

    // /// Creates the full URI for the HTTP request.
    // fn create_uri<'a>(
    //     &self,
    //     path: impl IntoIterator<Item = Cow<'a, str>>,
    // ) -> String {
    //     let mut res = String::from(self.base_uri.as_str());
    //     for item in path {
    //         if !res.ends_with('/') {
    //             res.push('/');
    //         }
    //         res.push_str(&item);
    //     }
    //     res
    // }
}

/// # High-level commands
///
impl NameshedClient {
    // pub async fn authorized(&self) -> Result<Success, Error> {
    //     self.get_ok(once("api/v1/authorized")).await
    // }

    // pub async fn info(&self) -> Result<api::admin::ServerInfo, Error> {
    //     self.get_json(once("stats/info")).await
    // }

    // pub async fn ca_add(&self, handle: CaHandle) -> Result<Success, Error> {
    //     self.post_json(
    //         once("api/v1/cas"),
    //         api::admin::CertAuthInit { handle },
    //     )
    //     .await
    // }

    // ...
}

//------------ Path Helpers --------------------------------------------------

// fn publisher_path(
//     publisher: &PublisherHandle,
// ) -> impl IntoIterator<Item = Cow<'_, str>> {
//     ["api/v1/pubd/publishers".into(), encode(publisher.as_str())]
// }

fn once(s: &str) -> impl Iterator<Item = Cow<'_, str>> {
    std::iter::once(s.into())
}

/// The set of ASCII characters that needs percent encoding in a path.
///
/// RFC 3986 defines the characters that do _not_ need encoding:
///
/// ```text
/// pchar         = unreserved / pct-encoded / sub-delims / ":" / "@"
/// unreserved    = ALPHA / DIGIT / "-" / "." / "_" / "~"
/// sub-delims    = "!" / "$" / "&" / "'" / "(" / ")"
///                 / "*" / "+" / "," / ";" / "="
/// ```
///
/// XXX Someone please double-check this.
const PATH_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}')
    .add(b'\x7f');

fn encode(s: &str) -> Cow<'_, str> {
    utf8_percent_encode(s, PATH_ENCODE_SET).into()
}
