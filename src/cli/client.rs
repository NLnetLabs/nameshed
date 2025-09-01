use std::time::Duration;

use reqwest::{IntoUrl, Method, RequestBuilder};
use url::Url;

const HTTP_CLIENT_TIMEOUT: Duration = Duration::from_secs(120);
static APP_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),);

pub struct NameshedApiClient {
    base_uri: Url,
}

impl NameshedApiClient {
    pub fn new(base_uri: impl IntoUrl) -> Self {
        NameshedApiClient {
            base_uri: base_uri.into_url().unwrap(),
        }
    }

    pub fn base_uri(&self) -> &str {
        self.base_uri.as_str()
    }

    pub fn request(&self, method: Method, s: &str) -> RequestBuilder {
        let path = self.base_uri.join(s).unwrap();

        let client = reqwest::ClientBuilder::new()
            .user_agent(APP_USER_AGENT)
            .timeout(HTTP_CLIENT_TIMEOUT)
            .build()
            .unwrap();

        client.request(method, path)
    }

    pub fn get(&self, s: &str) -> RequestBuilder {
        self.request(Method::GET, s)
    }

    pub fn post(&self, s: &str) -> RequestBuilder {
        self.request(Method::POST, s)
    }
}
